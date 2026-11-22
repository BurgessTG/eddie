//! The Voiceover panel, opened from the timeline toolbar's microphone: what
//! it records from, a level meter, whether the timeline plays aloud, and
//! the Record button with the countdown and the take's length. It floats
//! over the media column without blocking the window, so the playhead can
//! still be moved before recording.

use eddie_core::Action;
use eddie_core::editor::settings::Settings;
use eddie_core::shortcuts;
use rune::{Align, Cursor, El, Side, col, el, row, spacer, text};

use crate::app::Ui;
use crate::theme::{self, c};

const W: f32 = 288.0;
const BUTTON: f32 = 44.0;

/// Open the panel and the microphone behind its meter.
pub fn open(ui: &mut Ui) {
    if ui.editor.open_microphone() {
        ui.show_voiceover = true;
    }
}

/// Close the panel. A take still counting down or recording goes on; the
/// microphone closes after it.
pub fn close(ui: &mut Ui) {
    ui.show_voiceover = false;
}

pub fn toggle(ui: &mut Ui) {
    if ui.show_voiceover {
        close(ui);
    } else {
        open(ui);
    }
}

/// The meter's fill for a peak level: -60 dB to 0 dB across the bar.
pub fn meter_fill(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    ((20.0 * level.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn hear_timeline(ui: &mut Ui) {
    let before: Settings = ui.editor.settings.clone();
    ui.editor.settings.hear_timeline_while_recording ^= true;
    ui.editor.settings_changed(&before);
}

/// `seconds` as `0:04.2`.
fn clock(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0) as u64;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

/// The round Record button: a red dot, the countdown's number, or a red
/// square that stops the take.
fn record_button(ui: &Ui) -> El {
    let c = c();
    let voiceover = ui.editor.voiceover();
    let countdown = voiceover.and_then(|v| v.countdown());
    let recording = voiceover.is_some_and(|v| v.recording().is_some());
    let (glyph, help) = if let Some(left) = countdown {
        (
            text(left.to_string())
                .font(theme::semibold(18.0))
                .color(c.foreground)
                .single_line(),
            shortcuts::tip("Cancel the countdown", Action::RecordVoiceover),
        )
    } else if recording {
        (
            el().size(16.0, 16.0).radius(3.0).fill(c.destructive),
            shortcuts::tip(
                "Stop recording and put the take on the timeline",
                Action::RecordVoiceover,
            ),
        )
    } else {
        (
            el().size(22.0, 22.0).radius(11.0).fill(c.destructive),
            shortcuts::tip(
                "Record from the playhead after a 3-second countdown. Takes are saved beside the project",
                Action::RecordVoiceover,
            ),
        )
    };
    row()
        .key("voiceover-record")
        .size(BUTTON, BUTTON)
        .center()
        .radius(BUTTON / 2.0)
        .fill(c.secondary)
        .role("button")
        .label("Record")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .help(help)
        .hover(c.hover)
        .press(c.pressed)
        .on_press(|ui: &mut Ui| ui.act(Action::RecordVoiceover))
        .child(glyph)
}

/// The take's length while it records, else what the button will do.
fn readout(ui: &Ui) -> El {
    let c = c();
    let voiceover = ui.editor.voiceover();
    let recording = voiceover.and_then(|v| v.recording());
    let (time, state) = match (recording, voiceover.and_then(|v| v.countdown())) {
        (Some((_, take)), _) => (take.seconds(), "Recording".to_owned()),
        (None, Some(left)) => (0.0, format!("Starting in {left}")),
        (None, None) => (0.0, "Ready".to_owned()),
    };
    col()
        .gap(2.0)
        .child(
            text(clock(time))
                .key("voiceover-time")
                .font(theme::mono(18.0))
                .color(if recording.is_some() {
                    c.foreground
                } else {
                    c.muted_fg
                })
                .single_line(),
        )
        .child(theme::caption(state).key("voiceover-state").single_line())
}

/// The input's level, in green, amber past -6 dB and red past -1 dB.
fn meter(ui: &Ui) -> El {
    let c = c();
    let level = ui.editor.voiceover().map_or(0.0, |v| v.meter);
    let fill = meter_fill(level);
    let color = if level >= 0.89 {
        c.destructive
    } else if level >= 0.5 {
        c.warning
    } else {
        c.success
    };
    let width = W - 2.0 * 13.0;
    el().key("voiceover-meter")
        .w(width)
        .h(6.0)
        .radius(3.0)
        .fill(c.secondary)
        .instant_changes()
        .help("The microphone's level. Speak at a normal distance; red means it is too loud")
        .child(
            el().absolute(0.0, 0.0)
                .size(width * fill, 6.0)
                .radius(3.0)
                .fill(color),
        )
}

pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    if !ui.show_voiceover {
        return;
    }
    let c = c();
    let Some(voiceover) = ui.editor.voiceover() else {
        return;
    };
    let hear = ui.editor.settings.hear_timeline_while_recording;
    let input = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .help(if cfg!(target_os = "macos") {
            "Eddie records from the input chosen in System Settings → Sound"
        } else {
            "Eddie records from the system's default input; change it in your sound settings"
        })
        .child(theme::caption("Input"))
        .child(
            text(voiceover.microphone().to_owned())
                .key("voiceover-input")
                .font(theme::BODY)
                .color(c.foreground)
                .single_line()
                .truncate()
                .max_w(W - 90.0),
        );
    let card = theme::card()
        .key("voiceover-panel")
        .w(W)
        .gap(12.0)
        .shadow(4.0, 16.0, rune::Color::BLACK.alpha(120.0 / 255.0))
        .role("dialog")
        .label("Voiceover")
        .child(
            row()
                .w_full()
                .center_y()
                .child(theme::panel_title("Voiceover"))
                .child(spacer())
                .child(theme::close_button("voiceover-close", "Close (Esc)").on_press(close)),
        )
        .child(input)
        .child(meter(ui))
        .child(
            theme::checkbox(
                "voiceover-hear",
                "Hear the timeline",
                "Play the timeline aloud while recording. Use headphones, or the microphone records it too",
                hear,
            )
            .on_press(hear_timeline),
        )
        .child(
            row()
                .w_full()
                .gap(14.0)
                .center_y()
                .child(record_button(ui))
                .child(readout(ui)),
        )
        // Ending at the button, it covers the media column, not the
        // picture or the transport that recording is watched on.
        .anchored("tool-voiceover", Side::Top, Align::End, 8.0);
    overlays.push(card);
}

#[cfg(test)]
mod tests;

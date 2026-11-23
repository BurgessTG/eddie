//! The Export dialog: a video (its picture size, frame rate and quality),
//! the sound alone (its format) or a GIF (its size), then the file to
//! write. The choices are saved with the settings, so the next export and
//! `eddie export` start from them.

use eddie_core::editor::settings::Settings;
use openreel_media::{
    AudioFormat, ExportKind, ExportSettings, FRAME_RATES, GifSize, Quality, Resolution,
};
use rune::{El, col, el, row, text};

use crate::app::Ui;
use crate::theme::{self, Variant, c};

/// As wide as the longest label, "Frame rate".
const LABEL_W: f32 = 76.0;
const COLUMN_GAP: f32 = 16.0;
/// As wide as the six resolutions.
const CONTROL_W: f32 = 340.0;
const PAD: f32 = 21.0;
/// One setting's row, as tall as a segmented control.
const ROW_H: f32 = 32.0;
const ROW_GAP: f32 = 10.0;
/// The summary's line, 4 points further down than a setting's gap.
const SUMMARY_H: f32 = 20.0;
/// The settings and the summary under them, as tall for every kind as for
/// a video's four rows, so picking another kind never moves the dialog.
const BODY_H: f32 = 4.0 * ROW_H + 4.0 * ROW_GAP + SUMMARY_H;

/// Show the dialog, unless there is nothing to export or an export runs;
/// the status bar says which.
pub fn open(ui: &mut Ui) {
    if ui.editor.timeline.clips.is_empty() {
        ui.editor.status = "Nothing to export.".to_owned();
    } else if ui.editor.exporting() {
        ui.editor.status = "An export is already running.".to_owned();
    } else {
        crate::settings::close(ui);
        ui.show_shortcuts = false;
        ui.show_export = true;
    }
}

pub fn close(ui: &mut Ui) {
    ui.show_export = false;
}

/// Close the dialog and ask where to write the file.
pub fn confirm(ui: &mut Ui) {
    close(ui);
    ui.pick_export();
}

fn edit(ui: &mut Ui, change: impl FnOnce(&mut ExportSettings)) {
    let before: Settings = ui.editor.settings.clone();
    change(&mut ui.editor.settings.export);
    ui.editor.settings_changed(&before);
}

pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    if !ui.show_export {
        return;
    }
    let c = c();
    let settings = ui.editor.settings.export;
    let mut body = col()
        .key("export-settings")
        .w_full()
        .h(BODY_H)
        .gap(ROW_GAP)
        .child(setting("File", kinds(settings)));
    body = match settings.kind {
        ExportKind::Video => body
            .child(setting("Resolution", resolutions(ui, settings)))
            .child(setting("Frame rate", frame_rates(ui, settings)))
            .child(setting("Quality", qualities(settings))),
        ExportKind::Audio => body.child(setting("Format", audio_formats(settings))),
        ExportKind::Gif => body.child(setting("Size", gif_sizes(ui, settings))),
    };
    let body = body.child(
        row()
            .w_full()
            .h(SUMMARY_H)
            .pt(4.0)
            .child(el().w(LABEL_W + COLUMN_GAP))
            .child(
                theme::caption(summary(ui, settings))
                    .key("export-summary")
                    .single_line(),
            ),
    );
    let save = format!(
        "Choose where to save the {}, then export it (Enter)",
        settings.extension().to_ascii_uppercase()
    );
    let card = col()
        .key("export-dialog")
        .w(LABEL_W + COLUMN_GAP + CONTROL_W + 2.0 * PAD)
        .padding(PAD)
        .fill(c.card)
        .border(1.0, c.border)
        .radius(theme::R_LG)
        .shadow(8.0, 28.0, rune::Color::BLACK.alpha(140.0 / 255.0))
        .role("dialog")
        .label("Export")
        .modal(true, close)
        .child(header())
        .child(el().h(22.0))
        .child(body)
        .child(el().h(20.0))
        .child(theme::divider())
        .child(el().h(20.0))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(rune::spacer())
                .child(
                    theme::button(
                        "export-cancel",
                        "Cancel",
                        Variant::Secondary,
                        "Close without exporting (Esc)",
                        true,
                    )
                    .on_press(close),
                )
                .child(
                    theme::button("export-confirm", "Export…", Variant::Primary, &save, true)
                        .focusable(true)
                        .autofocus()
                        .default_button()
                        .focus_ring(2.0, 2.0, c.ring)
                        .on_press(confirm),
                ),
        );
    overlays.push(
        row()
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .child(card),
    );
}

fn header() -> El {
    row()
        .w_full()
        .h(28.0)
        .center_y()
        .child(
            el().pt(2.0).child(
                text("Export")
                    .font(theme::semibold(15.0))
                    .color(c().foreground)
                    .single_line(),
            ),
        )
        .child(rune::spacer())
        .child(theme::close_button("export-close", "Close without exporting (Esc)").on_press(close))
        .child(el().w(4.0))
}

/// A muted label beside its control.
fn setting(label: &str, control: El) -> El {
    row()
        .w_full()
        .h(ROW_H)
        .gap(COLUMN_GAP)
        .center_y()
        .child(
            text(label.to_owned())
                .font(theme::BODY)
                .color(c().muted_fg)
                .single_line()
                .w(LABEL_W),
        )
        .child(control)
}

/// What the export makes, under the settings.
fn summary(ui: &Ui, settings: ExportSettings) -> String {
    let ((width, height), fps) = ui.editor.export_output(settings);
    match settings.kind {
        ExportKind::Video => format!("{width} × {height} · {} fps", fps_text(fps)),
        ExportKind::Audio => {
            let format = settings.audio;
            let spec = match format {
                AudioFormat::Mp3 => "320 kbps",
                AudioFormat::Wav => "16-bit, 48 kHz",
                AudioFormat::Aac => "256 kbps, .m4a",
                AudioFormat::Flac => "lossless, 16-bit",
            };
            format!("{} · {spec} · no picture", format.label())
        }
        ExportKind::Gif => {
            format!("{width} × {height} · {} fps · no sound", fps_text(fps))
        }
    }
}

fn kinds(settings: ExportSettings) -> El {
    theme::segmented(
        "export-kind",
        ExportKind::ALL
            .into_iter()
            .map(|kind| {
                let help = match kind {
                    ExportKind::Video => "A video with its sound, as an MP4",
                    ExportKind::Audio => "The sound alone: MP3, WAV, AAC or FLAC",
                    ExportKind::Gif => "A silent looping GIF, best for a few seconds",
                };
                (
                    format!("export-kind-{}", kind.as_str()),
                    kind.label(),
                    help,
                    settings.kind == kind,
                )
            })
            .collect(),
        |index, segment| {
            let kind = ExportKind::ALL[index];
            segment.on_press(move |ui: &mut Ui| edit(ui, |export| export.kind = kind))
        },
    )
}

fn audio_formats(settings: ExportSettings) -> El {
    theme::segmented(
        "export-audio",
        AudioFormat::ALL
            .into_iter()
            .map(|format| {
                let help = match format {
                    AudioFormat::Mp3 => "Plays everywhere: 320 kbps MP3",
                    AudioFormat::Wav => "Uncompressed 16-bit WAV, for another editor",
                    AudioFormat::Aac => "256 kbps AAC in an .m4a, smaller than MP3",
                    AudioFormat::Flac => "Lossless, and smaller than WAV",
                };
                (
                    format!("export-audio-{}", format.as_str()),
                    format.label(),
                    help,
                    settings.audio == format,
                )
            })
            .collect(),
        |index, segment| {
            let format = AudioFormat::ALL[index];
            segment.on_press(move |ui: &mut Ui| edit(ui, |export| export.audio = format))
        },
    )
}

/// Each GIF size, its tooltip naming the size it makes of this project.
fn gif_sizes(ui: &Ui, settings: ExportSettings) -> El {
    let helps: Vec<String> = GifSize::ALL
        .into_iter()
        .map(|gif| {
            let ((width, height), _) = ui.editor.export_output(ExportSettings { gif, ..settings });
            format!("{width} × {height}")
        })
        .collect();
    theme::segmented(
        "export-gif",
        GifSize::ALL
            .into_iter()
            .zip(&helps)
            .map(|(gif, help)| {
                (
                    format!("export-gif-{}", gif.as_str()),
                    gif.as_str(),
                    help.as_str(),
                    settings.gif == gif,
                )
            })
            .collect(),
        |index, segment| {
            let gif = GifSize::ALL[index];
            segment.on_press(move |ui: &mut Ui| edit(ui, |export| export.gif = gif))
        },
    )
}

/// Each resolution, its tooltip naming the size it makes of this project.
fn resolutions(ui: &Ui, settings: ExportSettings) -> El {
    let helps: Vec<String> = Resolution::ALL
        .into_iter()
        .map(|resolution| {
            let ((width, height), _) = ui.editor.export_output(ExportSettings {
                resolution,
                ..settings
            });
            match resolution {
                Resolution::Auto => {
                    format!("The largest video's size, up to 1080p: {width} × {height}")
                }
                _ => format!("{width} × {height}"),
            }
        })
        .collect();
    theme::segmented(
        "export-resolution",
        Resolution::ALL
            .into_iter()
            .zip(&helps)
            .map(|(resolution, help)| {
                (
                    format!("export-resolution-{}", resolution.as_str()),
                    resolution.label(),
                    help.as_str(),
                    settings.resolution == resolution,
                )
            })
            .collect(),
        |index, segment| {
            let resolution = Resolution::ALL[index];
            segment.on_press(move |ui: &mut Ui| edit(ui, |export| export.resolution = resolution))
        },
    )
}

/// Each frame rate; the project's own is marked until another is picked.
fn frame_rates(ui: &Ui, settings: ExportSettings) -> El {
    let project = ui.editor.timeline.fps;
    let shown = settings.fps.map_or(project, f64::from);
    let labels: Vec<String> = FRAME_RATES.iter().map(u32::to_string).collect();
    let helps: Vec<String> = FRAME_RATES
        .iter()
        .map(|&fps| {
            let what = match fps {
                24 => "the film look",
                25 => "PAL television",
                30 => "most phones and the web",
                50 => "smooth motion, PAL",
                _ => "smooth motion",
            };
            if f64::from(fps) == project {
                format!("{fps} fps, {what}; the project's rate")
            } else {
                format!("{fps} fps, {what}")
            }
        })
        .collect();
    theme::segmented(
        "export-fps",
        FRAME_RATES
            .iter()
            .zip(labels.iter().zip(&helps))
            .map(|(&fps, (label, help))| {
                (
                    format!("export-fps-{fps}"),
                    label.as_str(),
                    help.as_str(),
                    f64::from(fps) == shown,
                )
            })
            .collect(),
        |index, segment| {
            let fps = FRAME_RATES[index];
            segment.on_press(move |ui: &mut Ui| edit(ui, |export| export.fps = Some(fps)))
        },
    )
}

fn qualities(settings: ExportSettings) -> El {
    theme::segmented(
        "export-quality",
        Quality::ALL
            .into_iter()
            .map(|quality| {
                let help = match quality {
                    Quality::Lower => "Smaller files with softer detail",
                    Quality::Recommended => "Clean detail at a sensible size",
                    Quality::Higher => "The most detail, in the largest files",
                };
                (
                    format!("export-quality-{}", quality.as_str()),
                    quality.label(),
                    help,
                    settings.quality == quality,
                )
            })
            .collect(),
        |index, segment| {
            let quality = Quality::ALL[index];
            segment.on_press(move |ui: &mut Ui| edit(ui, |export| export.quality = quality))
        },
    )
}

/// 30 as "30", 29.97 as "29.97".
fn fps_text(fps: f64) -> String {
    if fps.fract() == 0.0 {
        format!("{fps:.0}")
    } else {
        format!("{fps:.2}")
    }
}

#[cfg(test)]
mod tests;

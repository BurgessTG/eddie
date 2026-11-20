//! The Export dialog: the picture size, frame rate and quality of the MP4,
//! then the file to write. The choices are saved with the settings, so the
//! next export and `eddie export` start from them.

use eddie_core::editor::settings::Settings;
use openreel_media::{ExportSettings, FRAME_RATES, Quality, Resolution};
use rune::{El, col, el, row, text};

use crate::app::Ui;
use crate::theme::{self, Variant, c};

/// As wide as the longest label, "Frame rate".
const LABEL_W: f32 = 76.0;
const COLUMN_GAP: f32 = 16.0;
/// As wide as the six resolutions.
const CONTROL_W: f32 = 340.0;
const PAD: f32 = 21.0;

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
    let ((width, height), fps) = ui.editor.export_output(settings);
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
        .child(
            col()
                .w_full()
                .gap(10.0)
                .child(setting("Resolution", resolutions(ui, settings)))
                .child(setting("Frame rate", frame_rates(ui, settings)))
                .child(setting("Quality", qualities(settings))),
        )
        .child(el().h(14.0))
        .child(
            row().w_full().child(el().w(LABEL_W + COLUMN_GAP)).child(
                theme::caption(format!("{width} × {height} · {} fps", fps_text(fps)))
                    .key("export-summary")
                    .single_line(),
            ),
        )
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
                    theme::button(
                        "export-confirm",
                        "Export…",
                        Variant::Primary,
                        "Choose where to save the MP4, then export it (Enter)",
                        true,
                    )
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

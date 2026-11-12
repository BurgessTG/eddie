//! The Settings card: its header, one row per setting, and Done.

use eddie_core::editor::settings::Settings;
use eddie_core::editor::sweep::transcribe::Provider;
use openreel_engine::Orientation;
use rune::{El, col, el, row, text};

use super::{
    BEST_MODEL, COLUMN_GAP, CONTROL_W, Field, LABEL_W, PAD, ROW_GAP, close, edit, toggle_models,
};
use crate::app::Ui;
use crate::theme::{self, FIELD_H, Variant, c};

/// The card, centred over the window.
pub(super) fn centred(ui: &Ui) -> El {
    let c = c();
    let card = col()
        .key("settings")
        .w(LABEL_W + COLUMN_GAP + CONTROL_W + 2.0 * PAD)
        .padding(PAD)
        .fill(c.card)
        .border(1.0, c.border)
        .radius(theme::R_LG)
        .shadow(8.0, 28.0, rune::Color::BLACK.alpha(140.0 / 255.0))
        .role("dialog")
        .label("Settings")
        .modal(true, close)
        .child(header())
        .child(el().h(22.0))
        .child(rows(ui))
        .child(el().h(24.0))
        .child(theme::divider())
        .child(el().h(20.0))
        .child(row().w_full().child(rune::spacer()).child(done()));
    row()
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .center()
        .child(card)
}

fn header() -> El {
    row()
        .w_full()
        .h(28.0)
        .center_y()
        // Padding under centring: one pixel lower, where the title reads centred.
        .child(
            el().pt(2.0).child(
                text("Settings")
                    .font(theme::semibold(15.0))
                    .color(c().foreground)
                    .single_line(),
            ),
        )
        .child(rune::spacer())
        .child(theme::close_button("settings-close", "Close settings (Esc)").on_press(close))
        // The header is 4 narrower than the grid under it.
        .child(el().w(4.0))
}

/// Every setting, top to bottom. A cloud provider adds its URL and key.
fn rows(ui: &Ui) -> El {
    let settings = &ui.editor.settings;
    let mut rows = col()
        .w_full()
        .gap(ROW_GAP)
        .child(setting("Playback", hardware(settings)))
        .child(setting("Speech", provider(ui)))
        .child(model(ui));
    if settings.transcription.provider != Provider::Local {
        let (url_hint, key_hint) = match settings.transcription.provider {
            Provider::Meta if settings.has_meta_key() => {
                ("https://api.meta.ai/v1", "Saved Meta key")
            }
            Provider::Meta => ("https://api.meta.ai/v1", "Not set"),
            _ => ("https://api.openai.com/v1", "Not set"),
        };
        rows = rows
            .child(typed(
                ui,
                Field::Url,
                url_hint,
                "Transcription API base URL, such as https://api.groq.com/openai/v1",
            ))
            .child(typed(
                ui,
                Field::Key,
                key_hint,
                "Transcription API key. Stored only in the local app config.",
            ));
    }
    rows.child(typed(
        ui,
        Field::Language,
        "Detect",
        "Spoken language as a code such as en or es. Empty detects it.",
    ))
    .child(typed(
        ui,
        Field::Vocabulary,
        "Names to spell right",
        "A short, comma-separated list of names and terms, such as Eddie, Bothell. Local and OpenAI-compatible providers use it; it can also make other words worse.",
    ))
    .child(setting("Format", format(settings)))
}

fn hardware(settings: &Settings) -> El {
    theme::checkbox(
        "settings-hardware",
        "Hardware decode",
        &format!(
            "Play HEVC, AV1, VP9 and footage above 1440p on the GPU ({}) when available. Scrubbing stays on the CPU; a GPU error switches back to the CPU.",
            if cfg!(target_os = "macos") {
                "VideoToolbox"
            } else {
                "VAAPI"
            }
        ),
        settings.hardware_decode,
    )
    .on_press(|ui: &mut Ui| edit(ui, |settings| settings.hardware_decode = !settings.hardware_decode))
}

/// The speech provider; a press opens its list.
fn provider(ui: &Ui) -> El {
    theme::select(
        "settings-provider",
        ui.editor.settings.transcription.provider.label(),
        "Who turns speech into text. Local runs whisper on this computer with no key; cloud providers upload the audio.",
        theme::field_fill(),
        ui.settings.providers_open,
        true,
    )
    .w(CONTROL_W)
    .on_press(|ui: &mut Ui| ui.settings.providers_open = !ui.settings.providers_open)
}

/// The speech model row: a pick list of installed models for the local
/// provider, a typed model ID for a cloud one.
fn model(ui: &Ui) -> El {
    let speech = &ui.editor.settings.transcription;
    match speech.provider {
        Provider::Local => {
            let folder = ui.editor.model_dir.as_deref().map_or_else(
                || "the whisper folder".to_owned(),
                |dir| dir.display().to_string(),
            );
            let choice = if speech.model.is_empty() {
                BEST_MODEL
            } else {
                &speech.model
            };
            let select = theme::select(
                "settings-local-model",
                choice,
                &format!(
                    "The whisper model that transcribes on this computer. Models are ggml-<name>.bin files in {folder}."
                ),
                theme::field_fill(),
                ui.settings.models.is_some(),
                true,
            )
            .w(CONTROL_W)
            .on_press(toggle_models);
            setting(Field::Model.label(), select)
        }
        Provider::Meta => typed(
            ui,
            Field::Model,
            "muse-voice-transcribe-1.0",
            "Meta transcription model ID",
        ),
        Provider::OpenAi => typed(
            ui,
            Field::Model,
            "whisper-1",
            "Model ID. Word timing needs a whisper model, such as whisper-1 or Groq's whisper-large-v3.",
        ),
    }
}

fn format(settings: &Settings) -> El {
    let landscape = settings.format == Orientation::Landscape;
    theme::segmented(
        "settings-format",
        vec![
            (
                "settings-landscape".to_owned(),
                "Landscape",
                "Wide project; portrait video is not added",
                landscape,
            ),
            (
                "settings-portrait".to_owned(),
                "Portrait",
                "Tall project; landscape video is not added",
                !landscape,
            ),
        ],
        |index, segment| {
            let format = if index == 0 {
                Orientation::Landscape
            } else {
                Orientation::Portrait
            };
            segment.on_press(move |ui: &mut Ui| edit(ui, |settings| settings.format = format))
        },
    )
}

fn done() -> El {
    theme::button(
        "settings-done",
        "Done",
        Variant::Primary,
        "Close settings; changes save as you edit",
        true,
    )
    // Opening by keyboard lands here, so Space and Enter close the dialog
    // and no setting changes by accident.
    .focusable(true)
    .autofocus()
    .default_button()
    .focus_ring(2.0, 2.0, c().ring)
    .on_press(close)
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

/// A typed setting: `hint` shows while it is empty.
fn typed(ui: &Ui, field: Field, hint: &str, help: &str) -> El {
    let c = c();
    let index = field as usize;
    let input = rune::text_input(&ui.settings.edits[index], theme::BODY)
        .selection_color(theme::selection())
        .key(field.key())
        .single_line()
        .w(CONTROL_W)
        .h(FIELD_H)
        .px(8.0)
        .py(6.0)
        .radius(theme::R_MD)
        .fill(c.input)
        .color(c.foreground)
        .placeholder(hint.to_owned(), c.muted_fg)
        .focus_within_ring(1.0, -1.0, c.brand)
        .label(field.label())
        .help(help.to_owned())
        .on_edit(rune::edit_access(move |ui: &mut Ui| {
            &mut ui.settings.edits[index]
        }));
    setting(field.label(), input)
}

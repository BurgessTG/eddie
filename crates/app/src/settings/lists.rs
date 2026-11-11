//! The Settings pick lists: speech providers and installed local models.
//! Each drops under its select and shuts on a pick, Esc or a press outside.

use eddie_core::editor::sweep::transcribe::{self, Provider};
use rune::{Align, El, ScrollSpec, Side, col};
use rune_kit::{ScrollBar, ScrollBarStyle};

use super::{BEST_MODEL, CONTROL_W, MODEL_ROWS, Models, OPTION_GAP, edit};
use crate::app::Ui;
use crate::theme::{self, FIELD_H, c};

pub(super) fn providers(ui: &Ui, overlays: &mut Vec<El>) {
    let current = ui.editor.settings.transcription.provider;
    let shut = |ui: &mut Ui| ui.settings.providers_open = false;
    overlays.push(rune_kit::scrim("settings-providers-scrim", shut));
    overlays.push(
        theme::popup("settings-providers")
            .w(CONTROL_W)
            .gap(6.0)
            .children(Provider::ALL.into_iter().map(|provider| {
                theme::option(
                    provider_key(provider),
                    provider.label(),
                    provider_help(provider),
                    provider == current,
                )
                .on_press(move |ui: &mut Ui| {
                    ui.settings.providers_open = false;
                    edit(ui, |settings| settings.transcription.provider = provider);
                })
            }))
            .anchored("settings-provider", Side::Bottom, Align::Start, 0.0)
            .modal(true, shut),
    );
}

pub(super) fn provider_key(provider: Provider) -> String {
    match provider {
        Provider::Local => "settings-provider-local",
        Provider::Meta => "settings-provider-meta",
        Provider::OpenAi => "settings-provider-openai",
    }
    .to_owned()
}

fn provider_help(provider: Provider) -> &'static str {
    match provider {
        Provider::Local => "Transcribe on this computer with whisper.cpp; no key, nothing uploaded",
        Provider::Meta => {
            "Upload the audio to Meta Voice Transcribe; speaker turns, no word timing"
        }
        Provider::OpenAi => {
            "Upload the audio to an OpenAI-style transcription API, such as OpenAI or Groq"
        }
    }
}

/// Best installed, each installed model, then the saved choice when it is
/// a path or a model that is not installed.
pub(super) fn models(ui: &Ui, models: &Models, overlays: &mut Vec<El>) {
    let c = c();
    let saved = &ui.editor.settings.transcription.model;
    let shut = |ui: &mut Ui| ui.settings.models = None;
    let pick = |key: String, label: &str, help: &str, model: &str| {
        let model = model.to_owned();
        theme::option(key, label, help, *saved == model).on_press(move |ui: &mut Ui| {
            ui.settings.models = None;
            edit(ui, |settings| {
                settings.transcription.model.clone_from(&model);
            });
        })
    };
    let mut options = vec![pick(
        "settings-best-model".into(),
        BEST_MODEL,
        &best_help(models.best),
        "",
    )];
    options.extend(models.installed.iter().map(|name| {
        pick(
            format!("settings-model-{name}"),
            name,
            &format!("Transcribe with ggml-{name}.bin"),
            name,
        )
    }));
    if !saved.is_empty() && !models.installed.contains(saved) {
        options.push(pick(
            "settings-other-model".into(),
            saved,
            "The saved choice: a path, or a model that is not in the model folder",
            saved,
        ));
    }
    let count = options.len();
    let mut list = col().w_full().gap(OPTION_GAP).children(options);
    if count > MODEL_ROWS {
        let rows = MODEL_ROWS as f32;
        list = col()
            .key("settings-models-scroll")
            .w_full()
            .h(rows * FIELD_H + (rows - 1.0) * OPTION_GAP)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(list)
            .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside());
    }
    overlays.push(rune_kit::scrim("settings-models-scrim", shut));
    overlays.push(
        theme::popup("settings-models")
            .w(CONTROL_W)
            .child(list)
            .anchored("settings-local-model", Side::Bottom, Align::Start, 0.0)
            .modal(true, shut),
    );
}

/// What "Best installed" means in this build, naming the model it uses now.
fn best_help(best: Option<&str>) -> String {
    match best {
        Some(name) if transcribe::gpu_build() => {
            format!("Use the most accurate model that fits the GPU's free memory: {name} at best")
        }
        Some(name) => format!("Use the most accurate model this build runs well: {name} now"),
        None => "Use the most accurate model this build runs well; none is installed".into(),
    }
}

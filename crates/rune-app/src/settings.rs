//! The Settings dialog: playback, the speech provider and the project
//! format. `eddie_core` loads, saves and applies the settings; this file
//! draws the controls and copies what is typed into `Editor::settings`.

use eddie_core::editor::sweep::transcribe::{Provider, TranscribeSettings};
use openreel_engine::Orientation;
use rune::{Align, El, Side, TextEdit, col, el, row, text};

use crate::app::Ui;
use crate::theme::{self, FIELD_H, Variant, c};

/// As wide as the longest label, "Speech model".
const LABEL_W: f32 = 92.0;
const COLUMN_GAP: f32 = 16.0;
const CONTROL_W: f32 = 316.0;
const ROW_GAP: f32 = 10.0;
/// The card's padding, its border included.
const PAD: f32 = 21.0;

/// A typed setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Model,
    Url,
    Key,
    Language,
    Vocabulary,
}

impl Field {
    const ALL: [Self; 5] = [
        Self::Model,
        Self::Url,
        Self::Key,
        Self::Language,
        Self::Vocabulary,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Model => "settings-model",
            Self::Url => "settings-url",
            Self::Key => "settings-key",
            Self::Language => "settings-language",
            Self::Vocabulary => "settings-vocabulary",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Model => "Speech model",
            Self::Url => "Speech URL",
            Self::Key => "Speech key",
            Self::Language => "Language",
            Self::Vocabulary => "Vocabulary",
        }
    }

    fn value(self, speech: &mut TranscribeSettings) -> &mut String {
        match self {
            Self::Model => &mut speech.model,
            Self::Url => &mut speech.base_url,
            Self::Key => &mut speech.api_key,
            Self::Language => &mut speech.language,
            Self::Vocabulary => &mut speech.vocabulary,
        }
    }
}

pub struct State {
    edits: [TextEdit; 5],
    /// The fields that had the keyboard at the last poll. One the keyboard
    /// has just left still holds keystrokes the settings have not seen.
    typed: [bool; 5],
    /// The speech provider list is open.
    pub providers_open: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            edits: Field::ALL.map(|field| {
                let edit = TextEdit::new("");
                if field == Field::Key {
                    edit.secure()
                } else {
                    edit
                }
            }),
            typed: [false; 5],
            providers_open: false,
        }
    }
}

impl State {
    /// A field here has the keyboard, so plain keys are text.
    pub fn typing(&self) -> bool {
        self.edits.iter().any(|edit| edit.focused)
    }
}

/// Close the dialog. What was typed is kept by the next `sync`.
pub fn close(ui: &mut Ui) {
    ui.show_settings = false;
    ui.settings.providers_open = false;
}

/// Change the settings through `change`, then apply and save them.
fn edit(ui: &mut Ui, change: impl FnOnce(&mut eddie_core::editor::settings::Settings)) {
    let before = ui.editor.settings.clone();
    change(&mut ui.editor.settings);
    ui.editor.settings_changed(&before);
}

/// Once per frame, before the editor's own: a field being typed in goes to
/// the settings as typed, every other field shows its setting, and a
/// change is applied and saved.
pub fn sync(ui: &mut Ui) {
    let open = ui.show_settings;
    let held = ui.settings.typing() || ui.settings.typed.contains(&true);
    if !open && !held {
        return;
    }
    // Opening a project also sets the format, so `before` is read here.
    let before = ui.editor.settings.clone();
    let speech = &mut ui.editor.settings.transcription;
    for field in Field::ALL {
        let index = field as usize;
        let edit = &mut ui.settings.edits[index];
        let value = field.value(speech);
        if edit.focused || ui.settings.typed[index] {
            if *value != edit.text {
                value.clone_from(&edit.text);
            }
        } else if edit.text != *value {
            edit.replace_text(value.clone());
        }
        // A closed dialog has no fields; none may keep plain keys.
        edit.focused &= open;
        ui.settings.typed[index] = edit.focused;
    }
    ui.editor.settings_changed(&before);
}

/// The dialog, centred, and its open provider list. The editor behind is
/// inert; a press outside, Esc or Done closes it.
pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    if !ui.show_settings {
        return;
    }
    let c = c();
    let settings = &ui.editor.settings;
    let speech = &settings.transcription;
    let header = row()
        .w_full()
        .h(28.0)
        .center_y()
        // Padding under centring: one pixel lower, where egui sets it.
        .child(
            el().pt(2.0).child(
                text("Settings")
                    .font(theme::semibold(15.0))
                    .color(c.foreground)
                    .single_line(),
            ),
        )
        .child(rune::spacer())
        .child(theme::close_button("settings-close", "Close settings (Esc)").on_press(close))
        // egui's header is 4 narrower than its grid.
        .child(el().w(4.0));

    let hardware = theme::checkbox(
        "settings-hardware",
        "Hardware decode",
        "Play HEVC, AV1, VP9 and footage above 1440p on the GPU (VAAPI) when available. Scrubbing stays on the CPU; a GPU error switches back to the CPU.",
        settings.hardware_decode,
    )
    .on_press(|ui: &mut Ui| edit(ui, |settings| settings.hardware_decode = !settings.hardware_decode));
    let provider = theme::select(
        "settings-provider",
        speech.provider.label(),
        "Who turns speech into text. Local runs whisper on this computer with no key; cloud providers upload the audio.",
        theme::field_fill(),
        ui.settings.providers_open,
        true,
    )
    .w(CONTROL_W)
    .on_press(|ui: &mut Ui| ui.settings.providers_open = !ui.settings.providers_open);
    let (model_hint, model_help) = match speech.provider {
        Provider::Local => (
            "Best installed",
            "A whisper model name such as large-v3 or large-v3-turbo, or a path to a ggml file. Empty uses the most accurate one installed.",
        ),
        Provider::Meta => ("muse-voice-transcribe-1.0", "Meta transcription model ID"),
        Provider::OpenAi => (
            "whisper-1",
            "Model ID. Word timing needs a whisper model, such as whisper-1 or Groq's whisper-large-v3.",
        ),
    };

    let mut rows = col()
        .w_full()
        .gap(ROW_GAP)
        .child(setting("Playback", hardware))
        .child(setting("Speech", provider))
        .child(typed(ui, Field::Model, model_hint, model_help));
    if speech.provider != Provider::Local {
        let (url_hint, key_hint) = match speech.provider {
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
    let landscape = settings.format == Orientation::Landscape;
    let format = theme::segmented(
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
    );
    rows = rows
        .child(typed(
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
        .child(setting("Format", format));

    let done = theme::button(
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
    .focus_ring(2.0, 2.0, c.ring)
    .on_press(close);
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
        .child(header)
        .child(el().h(22.0))
        .child(rows)
        .child(el().h(24.0))
        .child(theme::divider())
        .child(el().h(20.0))
        .child(row().w_full().child(rune::spacer()).child(done));
    overlays.push(
        row()
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .child(card),
    );

    if ui.settings.providers_open {
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
                        provider == speech.provider,
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
}

fn provider_key(provider: Provider) -> String {
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

#[cfg(test)]
mod tests {
    use eddie_core::editor::settings::Settings;
    use rune::{Key, Mods, Runtime};

    use super::*;
    use crate::testing::{chord, click, isolated_config, press, shown, tooltip, window};

    /// The window with Settings opened by its shortcut.
    fn settings() -> Runtime<Ui> {
        let mut rt = window(Ui::headless());
        open(&mut rt);
        rt
    }

    fn open(rt: &mut Runtime<Ui>) {
        let ctrl = Mods {
            cmd: true,
            ..Mods::default()
        };
        chord(rt, Key::Command(','), ctrl);
        assert!(shown(rt, "settings"), "Ctrl+, opens Settings");
    }

    #[test]
    fn escape_enter_and_done_close_it_and_save_nothing() {
        isolated_config(
            "settings::tests::escape_enter_and_done_close_it_and_save_nothing",
            |path| {
                let mut rt = settings();
                let before = rt.state.editor.settings.clone();
                press(&mut rt, Key::Escape);
                assert!(!shown(&rt, "settings"), "Esc closes it");

                open(&mut rt);
                press(&mut rt, Key::Enter);
                assert!(!shown(&rt, "settings"), "Enter presses Done");

                open(&mut rt);
                click(&mut rt, "settings-done");
                assert!(!shown(&rt, "settings"), "Done closes it");

                assert_eq!(rt.state.editor.settings, before);
                assert!(!path.exists(), "nothing changed, so nothing is saved");
            },
        );
    }

    #[test]
    fn a_change_applies_at_once_and_is_saved() {
        isolated_config(
            "settings::tests::a_change_applies_at_once_and_is_saved",
            |_| {
                let mut rt = settings();
                click(&mut rt, "settings-portrait");
                assert_eq!(rt.state.editor.timeline.orientation, Orientation::Portrait);
                assert_eq!(Settings::load().format, Orientation::Portrait);
                assert_eq!(rt.state.editor.status, "Settings saved.");

                click(&mut rt, "settings-language");
                assert!(rt.state.typing(), "plain keys are text in a field");
                rt.paste("en");
                // Done takes the keyboard from the field in the same press.
                click(&mut rt, "settings-done");
                assert!(!shown(&rt, "settings"));
                assert!(!rt.state.typing(), "a closed dialog keeps no keys");
                assert_eq!(Settings::load().transcription.language, "en");

                open(&mut rt);
                assert_eq!(rt.state.settings.edits[Field::Language as usize].text, "en");
            },
        );
    }

    #[test]
    fn a_cloud_provider_asks_for_its_url_and_key() {
        isolated_config(
            "settings::tests::a_cloud_provider_asks_for_its_url_and_key",
            |_| {
                let mut rt = settings();
                assert!(!shown(&rt, "settings-url") && !shown(&rt, "settings-key"));

                click(&mut rt, "settings-provider");
                assert!(shown(&rt, "settings-provider-openai"));
                press(&mut rt, Key::Escape);
                assert!(
                    !shown(&rt, "settings-provider-openai"),
                    "Esc shuts the list"
                );
                assert!(shown(&rt, "settings"), "and leaves the dialog open");

                click(&mut rt, "settings-provider");
                click(&mut rt, "settings-provider-openai");
                assert!(!shown(&rt, "settings-provider-openai"), "a choice shuts it");
                assert_eq!(Settings::load().transcription.provider, Provider::OpenAi);
                assert!(shown(&rt, "settings-url") && shown(&rt, "settings-key"));

                click(&mut rt, "settings-key");
                rt.paste("fixture-key-only");
                press(&mut rt, Key::Escape);
                assert_eq!(Settings::load().transcription.api_key, "fixture-key-only");
            },
        );
    }

    #[test]
    fn every_setting_explains_itself() {
        isolated_config("settings::tests::every_setting_explains_itself", |path| {
            let mut ui = Ui::headless();
            // Set here, not through the dialog, so the cloud rows show and
            // nothing is saved.
            ui.editor.settings.transcription.provider = Provider::OpenAi;
            let mut rt = window(ui);
            open(&mut rt);
            for key in [
                "settings-close",
                "settings-hardware",
                "settings-provider",
                "settings-model",
                "settings-url",
                "settings-key",
                "settings-language",
                "settings-vocabulary",
                "settings-landscape",
                "settings-portrait",
                "settings-done",
            ] {
                assert!(
                    tooltip(&mut rt, key).is_some_and(|words| !words.is_empty()),
                    "{key} has no tooltip"
                );
            }
            click(&mut rt, "settings-provider");
            for provider in Provider::ALL {
                let key = provider_key(provider);
                assert!(
                    tooltip(&mut rt, &key).is_some_and(|words| !words.is_empty()),
                    "{key} has no tooltip"
                );
            }
            assert!(!path.exists(), "looking saves nothing");
        });
    }
}

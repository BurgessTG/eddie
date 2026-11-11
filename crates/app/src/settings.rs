//! The Settings dialog: playback, the speech provider and the project
//! format. `eddie_core` loads, saves and applies the settings; this file
//! draws the controls and copies what is typed into `Editor::settings`.

use eddie_core::editor::sweep::transcribe::{self, Provider, TranscribeSettings};
use openreel_engine::Orientation;
use rune::{Align, El, ScrollSpec, Side, TextEdit, col, el, row, text};
use rune_kit::{ScrollBar, ScrollBarStyle};

use crate::app::Ui;
use crate::theme::{self, FIELD_H, Variant, c};

/// As wide as the longest label, "Speech model".
const LABEL_W: f32 = 92.0;
const COLUMN_GAP: f32 = 16.0;
const CONTROL_W: f32 = 316.0;
const ROW_GAP: f32 = 10.0;
/// The card's padding, its border included.
const PAD: f32 = 21.0;
const OPTION_GAP: f32 = 6.0;
/// The local model list scrolls past this many choices.
const MODEL_ROWS: usize = 8;
/// The local model choice that saves no name.
const BEST_MODEL: &str = "Best installed";

/// The open local model list, as the model folder was when it opened.
pub struct Models {
    /// The installed model an empty setting uses.
    best: Option<&'static str>,
    installed: Vec<String>,
}

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
    /// The local model list, while it is open.
    pub models: Option<Models>,
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
            models: None,
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
    ui.settings.models = None;
}

/// Open the local model list on what is installed now, or shut it.
fn toggle_models(ui: &mut Ui) {
    ui.settings.models = match ui.settings.models {
        Some(_) => None,
        None => {
            let dir = ui.editor.model_dir.as_deref();
            Some(Models {
                best: transcribe::best_installed(dir),
                installed: transcribe::installed_models(dir),
            })
        }
    };
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
        // Padding under centring: one pixel lower, where the title reads centred.
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
        // The header is 4 narrower than the grid under it.
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
    let model = match speech.provider {
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
    };

    let mut rows = col()
        .w_full()
        .gap(ROW_GAP)
        .child(setting("Playback", hardware))
        .child(setting("Speech", provider))
        .child(model);
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

    if let Some(models) = &ui.settings.models {
        let shut = |ui: &mut Ui| ui.settings.models = None;
        let pick = |key: String, label: &str, help: &str, model: &str| {
            let model = model.to_owned();
            theme::option(key, label, help, speech.model == model).on_press(move |ui: &mut Ui| {
                ui.settings.models = None;
                edit(ui, |settings| {
                    settings.transcription.model.clone_from(&model);
                });
            })
        };
        let best = match models.best {
            Some(name) if transcribe::gpu_build() => {
                format!(
                    "Use the most accurate model that fits the GPU's free memory: {name} at best"
                )
            }
            Some(name) => format!("Use the most accurate model this build runs well: {name} now"),
            None => "Use the most accurate model this build runs well; none is installed".into(),
        };
        let mut options = vec![pick("settings-best-model".into(), BEST_MODEL, &best, "")];
        options.extend(models.installed.iter().map(|name| {
            pick(
                format!("settings-model-{name}"),
                name,
                &format!("Transcribe with ggml-{name}.bin"),
                name,
            )
        }));
        if !speech.model.is_empty() && !models.installed.contains(&speech.model) {
            options.push(pick(
                "settings-other-model".into(),
                &speech.model,
                "The saved choice: a path, or a model that is not in the model folder",
                &speech.model,
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
    use crate::testing::{
        chord, click, frames, input_arrives, isolated_config, press, shown, tooltip, window,
    };

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
    fn a_failed_save_is_shown() {
        isolated_config("settings::tests::a_failed_save_is_shown", |path| {
            // A directory where the file should be makes the rename fail.
            std::fs::create_dir_all(&path).unwrap();
            let mut rt = settings();
            click(&mut rt, "settings-hardware");
            let hardware = rt.state.editor.settings.hardware_decode;
            // The switch is process-wide; leave it as other tests expect.
            openreel_media::set_hardware_decode(false);
            assert!(hardware, "the change still applies to this session");
            assert_eq!(rt.state.editor.status, "Couldn't save settings.");
        });
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

    /// A model folder beside the isolated config file, holding empty files.
    fn model_folder(config: &std::path::Path, files: &[&str]) -> std::path::PathBuf {
        let folder = config.with_file_name("test-models");
        std::fs::create_dir_all(&folder).unwrap();
        for file in files {
            std::fs::write(folder.join(file), b"").unwrap();
        }
        folder
    }

    #[test]
    fn the_local_model_is_picked_from_the_installed_ones() {
        isolated_config(
            "settings::tests::the_local_model_is_picked_from_the_installed_ones",
            |path| {
                let mut ui = Ui::headless();
                ui.editor.model_dir = Some(model_folder(
                    &path,
                    &[
                        "ggml-base.bin",
                        "ggml-large-v3-turbo.bin",
                        "ggml-silero-v5.1.2.bin",
                    ],
                ));
                let mut rt = window(ui);
                open(&mut rt);
                assert!(
                    shown(&rt, "settings-local-model") && !shown(&rt, "settings-model"),
                    "a local model is picked, not typed"
                );

                click(&mut rt, "settings-local-model");
                for key in [
                    "settings-best-model",
                    "settings-model-large-v3-turbo",
                    "settings-model-base",
                ] {
                    assert!(
                        tooltip(&mut rt, key).is_some_and(|words| !words.is_empty()),
                        "{key} has no tooltip"
                    );
                }
                assert!(
                    !shown(&rt, "settings-model-silero-v5.1.2"),
                    "the voice detector is not a speech model"
                );
                assert!(!shown(&rt, "settings-other-model"));
                press(&mut rt, Key::Escape);
                assert!(!shown(&rt, "settings-best-model"), "Esc shuts the list");
                assert!(shown(&rt, "settings"), "and leaves the dialog open");
                assert!(!path.exists(), "looking saves nothing");

                click(&mut rt, "settings-local-model");
                click(&mut rt, "settings-model-base");
                assert!(!shown(&rt, "settings-model-base"), "a choice shuts it");
                assert_eq!(Settings::load().transcription.model, "base");

                click(&mut rt, "settings-local-model");
                click(&mut rt, "settings-best-model");
                assert_eq!(Settings::load().transcription.model, "");
            },
        );
    }

    #[test]
    fn a_saved_model_outside_the_folder_stays_a_choice() {
        isolated_config(
            "settings::tests::a_saved_model_outside_the_folder_stays_a_choice",
            |path| {
                let mut ui = Ui::headless();
                ui.editor.model_dir = Some(model_folder(&path, &["ggml-base.bin"]));
                ui.editor.settings.transcription.model = "/models/custom.bin".into();
                let mut rt = window(ui);
                open(&mut rt);
                assert!(
                    tooltip(&mut rt, "settings-local-model").is_some_and(|words| !words.is_empty())
                );
                click(&mut rt, "settings-local-model");
                assert!(tooltip(&mut rt, "settings-other-model").is_some());
                click(&mut rt, "settings-other-model");
                assert!(!shown(&rt, "settings-other-model"));
                assert_eq!(
                    rt.state.editor.settings.transcription.model,
                    "/models/custom.bin"
                );
                assert!(!path.exists(), "keeping the choice saves nothing");

                click(&mut rt, "settings-local-model");
                click(&mut rt, "settings-model-base");
                click(&mut rt, "settings-local-model");
                assert!(
                    !shown(&rt, "settings-other-model"),
                    "only the saved choice is listed from outside the folder"
                );
            },
        );
    }

    #[test]
    fn a_long_model_list_scrolls_inside_the_window() {
        isolated_config(
            "settings::tests::a_long_model_list_scrolls_inside_the_window",
            |path| {
                let names: Vec<String> = (10..30).map(|n| format!("ggml-model-{n}.bin")).collect();
                let files: Vec<&str> = names.iter().map(String::as_str).collect();
                let mut ui = Ui::headless();
                ui.editor.model_dir = Some(model_folder(&path, &files));
                let mut rt = window(ui);
                open(&mut rt);
                click(&mut rt, "settings-local-model");

                // 21 choices in a list eight rows tall, inside the window.
                let ([_, top, _, height], _) = rt.presented("settings-models").unwrap();
                let rows = MODEL_ROWS as f32;
                assert_eq!(height, rows * FIELD_H + (rows - 1.0) * OPTION_GAP + 14.0);
                assert!(top >= 0.0 && top + height <= 900.0, "{top} + {height}");
                let (_, last) = rt.find("settings-model-model-29").unwrap();
                assert!(last > top + height, "the last choice starts out of sight");

                let (x, y) = rt.find("settings-models-scroll").unwrap();
                input_arrives();
                rt.wheel(x, y, -2000.0);
                frames(&mut rt, 3);
                let (_, last) = rt.find("settings-model-model-29").unwrap();
                assert!(last > top && last < top + height, "the wheel reaches it");
                click(&mut rt, "settings-model-model-29");
                assert_eq!(Settings::load().transcription.model, "model-29");
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

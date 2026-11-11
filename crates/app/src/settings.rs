//! The Settings dialog: playback, the speech provider and the project
//! format. `eddie_core` loads, saves and applies the settings; this module
//! draws the controls and copies what is typed into `Editor::settings`.
//!
//! `dialog` is the card and its rows, `lists` the provider and local model
//! pick lists.

mod dialog;
mod lists;

use eddie_core::editor::sweep::transcribe::{self, TranscribeSettings};
use rune::{El, TextEdit};

use crate::app::Ui;

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

/// The dialog and whichever of its pick lists is open. The editor behind
/// is inert; a press outside, Esc or Done closes it.
pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    if !ui.show_settings {
        return;
    }
    overlays.push(dialog::centred(ui));
    if ui.settings.providers_open {
        lists::providers(ui, overlays);
    }
    if let Some(models) = &ui.settings.models {
        lists::models(ui, models, overlays);
    }
}

#[cfg(test)]
mod tests;

//! Keyboard shortcuts. One table drives key handling, menu labels,
//! tooltips and the Keyboard shortcuts window, so they cannot disagree.
//! It names keys itself, so any window toolkit can read it.

use crate::editor::Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyName {
    /// The character the key types, lowercase for letters.
    Char(char),
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
    Backspace,
    Escape,
    Function(u8),
}

/// A key with the modifiers that must be held, exactly: Shift+S is not S.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    /// Ctrl, or Cmd on macOS.
    pub command: bool,
    pub shift: bool,
    pub key: KeyName,
}

const fn plain(key: KeyName) -> Chord {
    Chord {
        command: false,
        shift: false,
        key,
    }
}

const fn shift(key: KeyName) -> Chord {
    Chord {
        command: false,
        shift: true,
        key,
    }
}

const fn cmd(key: char) -> Chord {
    Chord {
        command: true,
        shift: false,
        key: KeyName::Char(key),
    }
}

const fn cmd_shift(key: char) -> Chord {
    Chord {
        command: true,
        shift: true,
        key: KeyName::Char(key),
    }
}

const fn letter(key: char) -> Chord {
    plain(KeyName::Char(key))
}

pub struct Binding {
    pub action: Action,
    pub group: &'static str,
    pub label: &'static str,
    /// The first chord is the one menus and tooltips show.
    pub keys: &'static [Chord],
    /// Held keys repeat (stepping); toggles fire once per press.
    pub repeats: bool,
    /// Also works while a text field has focus. Only Ctrl chords whose
    /// keys mean nothing to text editing qualify.
    pub in_text: bool,
}

const fn bind(
    action: Action,
    group: &'static str,
    label: &'static str,
    keys: &'static [Chord],
) -> Binding {
    Binding {
        action,
        group,
        label,
        keys,
        repeats: false,
        in_text: false,
    }
}

const fn repeating(binding: Binding) -> Binding {
    Binding {
        repeats: true,
        ..binding
    }
}

const fn anywhere(binding: Binding) -> Binding {
    Binding {
        in_text: true,
        ..binding
    }
}

pub const GROUPS: &[&str] = &["Project", "Edit", "Playback", "View"];

pub const BINDINGS: &[Binding] = &[
    anywhere(bind(
        Action::NewProject,
        "Project",
        "New project",
        &[cmd('n')],
    )),
    anywhere(bind(
        Action::OpenProject,
        "Project",
        "Open project",
        &[cmd('o')],
    )),
    anywhere(bind(
        Action::SaveProject,
        "Project",
        "Save project",
        &[cmd('s')],
    )),
    anywhere(bind(
        Action::SaveProjectAs,
        "Project",
        "Save project as",
        &[cmd_shift('s')],
    )),
    anywhere(bind(Action::Import, "Project", "Import media", &[cmd('i')])),
    anywhere(bind(Action::Export, "Project", "Export MP4", &[cmd('e')])),
    bind(Action::Undo, "Edit", "Undo", &[cmd('z')]),
    bind(Action::Redo, "Edit", "Redo", &[cmd_shift('z'), cmd('y')]),
    bind(
        Action::SplitSelected,
        "Edit",
        "Split at playhead",
        &[letter('s'), letter('b')],
    ),
    bind(
        Action::DeleteSelected,
        "Edit",
        "Delete clip",
        &[plain(KeyName::Delete), plain(KeyName::Backspace)],
    ),
    bind(Action::AddTitle, "Edit", "Add title", &[letter('t')]),
    bind(
        Action::ToggleMagnet,
        "Edit",
        "Magnet on or off",
        &[letter('m')],
    ),
    bind(
        Action::Deselect,
        "Edit",
        "Deselect",
        &[plain(KeyName::Escape)],
    ),
    bind(
        Action::PlayPause,
        "Playback",
        "Play or pause",
        &[plain(KeyName::Space)],
    ),
    bind(
        Action::ShuttleReverse,
        "Playback",
        "Reverse, faster each press",
        &[letter('j')],
    ),
    bind(Action::ShuttleStop, "Playback", "Stop", &[letter('k')]),
    bind(
        Action::ShuttleForward,
        "Playback",
        "Forward, faster each press",
        &[letter('l')],
    ),
    repeating(bind(
        Action::StepBack,
        "Playback",
        "Back one frame",
        &[plain(KeyName::Left)],
    )),
    repeating(bind(
        Action::StepForward,
        "Playback",
        "Forward one frame",
        &[plain(KeyName::Right)],
    )),
    repeating(bind(
        Action::StepBackSecond,
        "Playback",
        "Back one second",
        &[shift(KeyName::Left)],
    )),
    repeating(bind(
        Action::StepForwardSecond,
        "Playback",
        "Forward one second",
        &[shift(KeyName::Right)],
    )),
    repeating(bind(
        Action::PrevEdit,
        "Playback",
        "Previous cut",
        &[plain(KeyName::Up), letter('[')],
    )),
    repeating(bind(
        Action::NextEdit,
        "Playback",
        "Next cut",
        &[plain(KeyName::Down), letter(']')],
    )),
    bind(
        Action::GoStart,
        "Playback",
        "Go to start",
        &[plain(KeyName::Home)],
    ),
    bind(
        Action::GoEnd,
        "Playback",
        "Go to end",
        &[plain(KeyName::End)],
    ),
    // Ctrl and + is Shift+= on most layouts, so accept both spellings.
    repeating(anywhere(bind(
        Action::ZoomIn,
        "View",
        "Zoom in",
        &[cmd('='), cmd('+'), cmd_shift('+'), cmd_shift('=')],
    ))),
    repeating(anywhere(bind(
        Action::ZoomOut,
        "View",
        "Zoom out",
        &[cmd('-')],
    ))),
    bind(
        Action::ZoomFit,
        "View",
        "Fit timeline",
        &[shift(KeyName::Char('z'))],
    ),
    anywhere(bind(Action::OpenSettings, "View", "Settings", &[cmd(',')])),
    anywhere(bind(
        Action::ShowShortcuts,
        "View",
        "Keyboard shortcuts",
        &[cmd('/'), plain(KeyName::Function(1))],
    )),
];

fn binding(action: Action) -> Option<&'static Binding> {
    BINDINGS.iter().find(|binding| binding.action == action)
}

/// The command modifier's name: Cmd on a Mac, Ctrl elsewhere.
pub const COMMAND: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// A chord as a menu or keycap shows it, such as `Ctrl+Shift+S`.
pub fn format(chord: &Chord) -> String {
    let mut text = String::new();
    if chord.command {
        text.push_str(COMMAND);
        text.push('+');
    }
    if chord.shift {
        text.push_str("Shift+");
    }
    match chord.key {
        KeyName::Char('=' | '+') => text.push('+'),
        KeyName::Char(key) => text.extend(key.to_uppercase()),
        KeyName::Space => text.push_str("Space"),
        KeyName::Left => text.push('←'),
        KeyName::Right => text.push('→'),
        KeyName::Up => text.push('↑'),
        KeyName::Down => text.push('↓'),
        KeyName::Home => text.push_str("Home"),
        KeyName::End => text.push_str("End"),
        KeyName::Delete => text.push_str("Del"),
        KeyName::Backspace => text.push_str("Backspace"),
        KeyName::Escape => text.push_str("Esc"),
        KeyName::Function(number) => text.push_str(&format!("F{number}")),
    }
    text
}

/// The chords the Keyboard shortcuts window lists for a binding. Zoom's
/// Plus spellings are one key on the keyboard, so it shows once.
pub fn keycaps(binding: &Binding) -> Vec<String> {
    binding
        .keys
        .iter()
        .enumerate()
        .filter(|(index, chord)| {
            *index == 0
                || !(chord.key == KeyName::Char('+')
                    || (chord.key == KeyName::Char('=') && chord.shift))
        })
        .map(|(_, chord)| format(chord))
        .collect()
}

/// The chord a menu shows for `action`, or "" when it has none.
pub fn label(action: Action) -> String {
    binding(action)
        .and_then(|binding| binding.keys.first())
        .map(format)
        .unwrap_or_default()
}

/// `text` followed by the action's chord in parentheses, for tooltips.
pub fn tip(text: &str, action: Action) -> String {
    match label(action) {
        chord if chord.is_empty() => text.to_owned(),
        chord => format!("{text} ({chord})"),
    }
}

/// The action a pressed chord asks for. `typing` leaves plain keys and
/// text undo to the focused text field; `repeat` is a held key repeating.
pub fn action_for(chord: Chord, typing: bool, repeat: bool) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|binding| {
            (!typing || binding.in_text)
                && (!repeat || binding.repeats)
                && binding.keys.contains(&chord)
        })
        .map(|binding| binding.action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_match_exactly_and_text_fields_keep_plain_keys() {
        let s = letter('s');
        assert_eq!(action_for(s, false, false), Some(Action::SplitSelected));
        assert_eq!(
            action_for(shift(KeyName::Char('s')), false, false),
            None,
            "Shift+S is not split"
        );
        assert_eq!(action_for(cmd('s'), true, false), Some(Action::SaveProject));
        assert_eq!(action_for(s, true, false), None, "typing keeps plain keys");
        assert_eq!(
            action_for(cmd('z'), true, false),
            None,
            "a text field keeps its own undo"
        );
    }

    #[test]
    fn only_stepping_keys_repeat_while_held() {
        let left = plain(KeyName::Left);
        assert_eq!(action_for(left, false, true), Some(Action::StepBack));
        assert_eq!(action_for(plain(KeyName::Space), false, true), None);
    }

    #[test]
    fn every_chord_belongs_to_one_action_and_every_group_is_listed() {
        for (index, binding) in BINDINGS.iter().enumerate() {
            assert!(GROUPS.contains(&binding.group), "{}", binding.label);
            for chord in binding.keys {
                let owner = BINDINGS.iter().position(|other| other.keys.contains(chord));
                assert_eq!(owner, Some(index), "{} shares a chord", binding.label);
            }
        }
        // Cmd on a Mac, Ctrl elsewhere.
        let m = COMMAND;
        assert_eq!(label(Action::SaveProjectAs), format!("{m}+Shift+S"));
        assert_eq!(tip("Zoom in", Action::ZoomIn), format!("Zoom in ({m}++)"));
        assert_eq!(tip("Clear", Action::ClearTimeline), "Clear");
        let zoom_in = binding(Action::ZoomIn).expect("zoom in");
        assert_eq!(keycaps(zoom_in), [format!("{m}++")]);
        let redo = binding(Action::Redo).expect("redo");
        assert_eq!(keycaps(redo), [format!("{m}+Shift+Z"), format!("{m}+Y")]);
    }
}

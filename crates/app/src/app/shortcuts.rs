//! Keyboard shortcuts. One table drives key handling, menu labels,
//! tooltips and the Keyboard shortcuts window, so they cannot disagree.

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, RichText};

use super::UiAction;
use crate::theme;

const NONE: Modifiers = Modifiers::NONE;
const SHIFT: Modifiers = Modifiers::SHIFT;
const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::COMMAND
};

const fn key(modifiers: Modifiers, key: Key) -> KeyboardShortcut {
    KeyboardShortcut::new(modifiers, key)
}

pub(super) struct Binding {
    pub action: UiAction,
    pub group: &'static str,
    pub label: &'static str,
    /// The first chord is the one menus and tooltips show.
    pub keys: &'static [KeyboardShortcut],
    /// Held keys repeat (stepping); toggles fire once per press.
    pub repeats: bool,
    /// Also works while a text field has focus. Only Ctrl chords whose
    /// keys mean nothing to text editing qualify.
    pub in_text: bool,
}

const fn bind(
    action: UiAction,
    group: &'static str,
    label: &'static str,
    keys: &'static [KeyboardShortcut],
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

pub(super) const BINDINGS: &[Binding] = &[
    anywhere(bind(
        UiAction::NewProject,
        "Project",
        "New project",
        &[key(CMD, Key::N)],
    )),
    anywhere(bind(
        UiAction::OpenProject,
        "Project",
        "Open project",
        &[key(CMD, Key::O)],
    )),
    anywhere(bind(
        UiAction::SaveProject,
        "Project",
        "Save project",
        &[key(CMD, Key::S)],
    )),
    anywhere(bind(
        UiAction::SaveProjectAs,
        "Project",
        "Save project as",
        &[key(CMD_SHIFT, Key::S)],
    )),
    anywhere(bind(
        UiAction::Import,
        "Project",
        "Import media",
        &[key(CMD, Key::I)],
    )),
    anywhere(bind(
        UiAction::Export,
        "Project",
        "Export MP4",
        &[key(CMD, Key::E)],
    )),
    bind(UiAction::Undo, "Edit", "Undo", &[key(CMD, Key::Z)]),
    bind(
        UiAction::Redo,
        "Edit",
        "Redo",
        &[key(CMD_SHIFT, Key::Z), key(CMD, Key::Y)],
    ),
    bind(
        UiAction::SplitSelected,
        "Edit",
        "Split at playhead",
        &[key(NONE, Key::S), key(NONE, Key::B)],
    ),
    bind(
        UiAction::DeleteSelected,
        "Edit",
        "Delete clip",
        &[key(NONE, Key::Delete), key(NONE, Key::Backspace)],
    ),
    bind(
        UiAction::AddTitle,
        "Edit",
        "Add title",
        &[key(NONE, Key::T)],
    ),
    bind(
        UiAction::ToggleMagnet,
        "Edit",
        "Magnet on or off",
        &[key(NONE, Key::M)],
    ),
    bind(
        UiAction::Deselect,
        "Edit",
        "Deselect",
        &[key(NONE, Key::Escape)],
    ),
    bind(
        UiAction::PlayPause,
        "Playback",
        "Play or pause",
        &[key(NONE, Key::Space)],
    ),
    bind(
        UiAction::ShuttleReverse,
        "Playback",
        "Reverse, faster each press",
        &[key(NONE, Key::J)],
    ),
    bind(
        UiAction::ShuttleStop,
        "Playback",
        "Stop",
        &[key(NONE, Key::K)],
    ),
    bind(
        UiAction::ShuttleForward,
        "Playback",
        "Forward, faster each press",
        &[key(NONE, Key::L)],
    ),
    repeating(bind(
        UiAction::StepBack,
        "Playback",
        "Back one frame",
        &[key(NONE, Key::ArrowLeft)],
    )),
    repeating(bind(
        UiAction::StepForward,
        "Playback",
        "Forward one frame",
        &[key(NONE, Key::ArrowRight)],
    )),
    repeating(bind(
        UiAction::StepBackSecond,
        "Playback",
        "Back one second",
        &[key(SHIFT, Key::ArrowLeft)],
    )),
    repeating(bind(
        UiAction::StepForwardSecond,
        "Playback",
        "Forward one second",
        &[key(SHIFT, Key::ArrowRight)],
    )),
    repeating(bind(
        UiAction::PrevEdit,
        "Playback",
        "Previous cut",
        &[key(NONE, Key::ArrowUp), key(NONE, Key::OpenBracket)],
    )),
    repeating(bind(
        UiAction::NextEdit,
        "Playback",
        "Next cut",
        &[key(NONE, Key::ArrowDown), key(NONE, Key::CloseBracket)],
    )),
    bind(
        UiAction::GoStart,
        "Playback",
        "Go to start",
        &[key(NONE, Key::Home)],
    ),
    bind(
        UiAction::GoEnd,
        "Playback",
        "Go to end",
        &[key(NONE, Key::End)],
    ),
    // Ctrl and + is Shift+= on most layouts, so accept both spellings.
    repeating(anywhere(bind(
        UiAction::ZoomIn,
        "View",
        "Zoom in",
        &[
            key(CMD, Key::Equals),
            key(CMD, Key::Plus),
            key(CMD_SHIFT, Key::Plus),
            key(CMD_SHIFT, Key::Equals),
        ],
    ))),
    repeating(anywhere(bind(
        UiAction::ZoomOut,
        "View",
        "Zoom out",
        &[key(CMD, Key::Minus)],
    ))),
    bind(
        UiAction::ZoomFit,
        "View",
        "Fit timeline",
        &[key(SHIFT, Key::Z)],
    ),
    anywhere(bind(
        UiAction::FocusChat,
        "View",
        "Chat",
        &[key(CMD, Key::K)],
    )),
    anywhere(bind(
        UiAction::ShowShortcuts,
        "View",
        "Keyboard shortcuts",
        &[key(CMD, Key::Slash), key(NONE, Key::F1)],
    )),
];

fn binding(action: UiAction) -> Option<&'static Binding> {
    BINDINGS.iter().find(|binding| binding.action == action)
}

fn format(ctx: &egui::Context, shortcut: &KeyboardShortcut) -> String {
    let mac = ctx.os().is_mac();
    let mods = shortcut.modifiers;
    let mut text = String::new();
    if mods.command || mods.ctrl || mods.mac_cmd {
        text.push_str(if mac { "Cmd+" } else { "Ctrl+" });
    }
    if mods.alt {
        text.push_str(if mac { "Option+" } else { "Alt+" });
    }
    if mods.shift {
        text.push_str("Shift+");
    }
    text.push_str(match shortcut.logical_key {
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::Equals | Key::Plus => "+",
        Key::Minus => "-",
        Key::Slash => "/",
        Key::Escape => "Esc",
        Key::Delete => "Del",
        other => other.name(),
    });
    text
}

/// The chord a menu shows for `action`, or "" when it has none.
pub(super) fn label(ctx: &egui::Context, action: UiAction) -> String {
    binding(action)
        .and_then(|binding| binding.keys.first())
        .map(|shortcut| format(ctx, shortcut))
        .unwrap_or_default()
}

/// `text` followed by the action's chord in parentheses, for tooltips.
pub(super) fn tip(ctx: &egui::Context, text: &str, action: UiAction) -> String {
    match label(ctx, action) {
        chord if chord.is_empty() => text.to_owned(),
        chord => format!("{text} ({chord})"),
    }
}

/// Consume the first key press this frame that a binding owns. `typing`
/// leaves plain keys and text undo to the focused text field.
pub(super) fn take_action(ctx: &egui::Context, typing: bool) -> Option<UiAction> {
    ctx.input_mut(|input| {
        let mut found = None;
        input.events.retain(|event| {
            if found.is_some() {
                return true;
            }
            let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            let hit = BINDINGS.iter().find(|binding| {
                (!typing || binding.in_text)
                    && (!*repeat || binding.repeats)
                    && binding.keys.iter().any(|shortcut| {
                        shortcut.logical_key == *key && modifiers.matches_exact(shortcut.modifiers)
                    })
            });
            match hit {
                Some(binding) => {
                    found = Some(binding.action);
                    false
                }
                None => true,
            }
        });
        found
    })
}

/// One chord as a keycap chip.
fn keycap(ui: &mut egui::Ui, text: String) {
    egui::Frame::new()
        .fill(theme::SECONDARY)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER_STRONG))
        .corner_radius(egui::CornerRadius::same(theme::R_SM))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(12.0));
        });
}

fn group(ui: &mut egui::Ui, ctx: &egui::Context, name: &str) {
    ui.label(RichText::new(name).family(theme::semibold()));
    ui.add_space(4.0);
    for binding in BINDINGS.iter().filter(|binding| binding.group == name) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.set_min_height(24.0);
            ui.label(RichText::new(binding.label).color(theme::MUTED_FG));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // Zoom's Plus spellings are one key on the keyboard; show it once.
                let shown = binding.keys.iter().enumerate().filter(|(index, shortcut)| {
                    *index == 0
                        || !(shortcut.logical_key == Key::Plus
                            || (shortcut.logical_key == Key::Equals && shortcut.modifiers.shift))
                });
                let chords: Vec<String> =
                    shown.map(|(_, shortcut)| format(ctx, shortcut)).collect();
                for chord in chords.into_iter().rev() {
                    keycap(ui, chord);
                }
            });
        });
    }
}

/// The Keyboard shortcuts window: two columns grouped like the menus.
pub(super) fn window(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("Keyboard shortcuts")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(640.0, 0.0))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.x = 32.0;
            ui.columns(2, |columns| {
                group(&mut columns[0], ctx, "Project");
                columns[0].add_space(12.0);
                group(&mut columns[0], ctx, "Edit");
                group(&mut columns[1], ctx, "Playback");
                columns[1].add_space(12.0);
                group(&mut columns[1], ctx, "View");
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(
        ctx: &egui::Context,
        modifiers: Modifiers,
        key: Key,
        typing: bool,
    ) -> Option<UiAction> {
        let mut found = None;
        // Release in the same frame, or egui reads the next press as a repeat.
        let event = |pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        let input = egui::RawInput {
            events: vec![event(true), event(false)],
            modifiers,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| found = take_action(ctx, typing));
        found
    }

    #[test]
    fn every_chord_belongs_to_one_action() {
        let mut seen = Vec::new();
        for binding in BINDINGS {
            for shortcut in binding.keys {
                let chord = (shortcut.logical_key, shortcut.modifiers);
                assert!(!seen.contains(&chord), "{chord:?} is bound twice");
                seen.push(chord);
            }
        }
    }

    #[test]
    fn modifiers_must_match_exactly_and_typing_keeps_plain_keys() {
        let ctx = egui::Context::default();
        assert_eq!(
            press(&ctx, NONE, Key::S, false),
            Some(UiAction::SplitSelected)
        );
        assert_eq!(press(&ctx, CMD, Key::S, false), Some(UiAction::SaveProject));
        assert_eq!(
            press(&ctx, CMD_SHIFT, Key::S, false),
            Some(UiAction::SaveProjectAs)
        );
        assert_eq!(
            press(&ctx, SHIFT, Key::S, false),
            None,
            "Shift+S is not split"
        );
        assert_eq!(
            press(&ctx, SHIFT, Key::ArrowLeft, false),
            Some(UiAction::StepBackSecond)
        );
        assert_eq!(
            press(&ctx, CMD_SHIFT, Key::Plus, false),
            Some(UiAction::ZoomIn)
        );
        // fframes' bracket keys reach the same cut navigation as the arrows.
        assert_eq!(
            press(&ctx, NONE, Key::OpenBracket, false),
            Some(UiAction::PrevEdit)
        );
        assert_eq!(
            press(&ctx, NONE, Key::CloseBracket, false),
            Some(UiAction::NextEdit)
        );
        // A focused text field owns letters, arrows and its own undo.
        assert_eq!(press(&ctx, NONE, Key::S, true), None);
        assert_eq!(press(&ctx, NONE, Key::Space, true), None);
        assert_eq!(press(&ctx, CMD, Key::Z, true), None);
        assert_eq!(press(&ctx, CMD, Key::S, true), Some(UiAction::SaveProject));
        assert_eq!(press(&ctx, CMD, Key::K, true), Some(UiAction::FocusChat));
    }
}

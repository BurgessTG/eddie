//! Keyboard shortcuts for the egui window. The table, labels and matching
//! rules live in `eddie_core::shortcuts`; this file reads egui's key
//! events and draws the Keyboard shortcuts window.

use eddie_core::shortcuts::{self as table, Chord, KeyName, BINDINGS};
use eframe::egui::{self, Key, Modifiers, RichText};

use super::Action;
use crate::theme;

/// egui's key as the shared table names it. Keys no binding uses are `None`.
fn key_name(key: Key) -> Option<KeyName> {
    Some(match key {
        Key::Space => KeyName::Space,
        Key::ArrowLeft => KeyName::Left,
        Key::ArrowRight => KeyName::Right,
        Key::ArrowUp => KeyName::Up,
        Key::ArrowDown => KeyName::Down,
        Key::Home => KeyName::Home,
        Key::End => KeyName::End,
        Key::Delete => KeyName::Delete,
        Key::Backspace => KeyName::Backspace,
        Key::Escape => KeyName::Escape,
        Key::F1 => KeyName::Function(1),
        Key::Equals => KeyName::Char('='),
        Key::Plus => KeyName::Char('+'),
        Key::Minus => KeyName::Char('-'),
        Key::Comma => KeyName::Char(','),
        Key::Slash => KeyName::Char('/'),
        Key::OpenBracket => KeyName::Char('['),
        Key::CloseBracket => KeyName::Char(']'),
        other => {
            // Letters are named by their capital: "S".
            let mut name = other.name().chars();
            match (name.next(), name.next()) {
                (Some(letter), None) if letter.is_ascii_uppercase() => {
                    KeyName::Char(letter.to_ascii_lowercase())
                }
                _ => return None,
            }
        }
    })
}

/// A pressed key as a chord, when its modifiers are ones the table uses.
fn chord(key: Key, modifiers: Modifiers) -> Option<Chord> {
    let command = modifiers.matches_exact(Modifiers::COMMAND)
        || modifiers.matches_exact(Modifiers::COMMAND | Modifiers::SHIFT);
    let plain =
        modifiers.matches_exact(Modifiers::NONE) || modifiers.matches_exact(Modifiers::SHIFT);
    if !command && !plain {
        return None;
    }
    Some(Chord {
        command,
        shift: modifiers.shift,
        key: key_name(key)?,
    })
}

/// The chord a menu shows for `action`, or "" when it has none.
pub(super) fn label(_ctx: &egui::Context, action: Action) -> String {
    table::label(action)
}

/// `text` followed by the action's chord in parentheses, for tooltips.
pub(super) fn tip(_ctx: &egui::Context, text: &str, action: Action) -> String {
    table::tip(text, action)
}

/// Consume the first key press this frame that a binding owns. `typing`
/// leaves plain keys and text undo to the focused text field.
pub(super) fn take_action(ctx: &egui::Context, typing: bool) -> Option<Action> {
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
            found =
                chord(*key, *modifiers).and_then(|chord| table::action_for(chord, typing, *repeat));
            found.is_none()
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

fn group(ui: &mut egui::Ui, name: &str) {
    ui.label(RichText::new(name).family(theme::semibold()));
    ui.add_space(4.0);
    for binding in BINDINGS.iter().filter(|binding| binding.group == name) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.set_min_height(24.0);
            ui.label(RichText::new(binding.label).color(theme::MUTED_FG));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let chords = table::keycaps(binding);
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
                group(&mut columns[0], "Project");
                columns[0].add_space(12.0);
                group(&mut columns[0], "Edit");
                group(&mut columns[1], "Playback");
                columns[1].add_space(12.0);
                group(&mut columns[1], "View");
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(ctx: &egui::Context, modifiers: Modifiers, key: Key, typing: bool) -> Option<Action> {
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

    const NONE: Modifiers = Modifiers::NONE;
    const SHIFT: Modifiers = Modifiers::SHIFT;
    const CMD: Modifiers = Modifiers::COMMAND;
    const CMD_SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::COMMAND
    };

    #[test]
    fn every_bound_key_has_an_egui_key() {
        // A binding whose key egui cannot name would never fire here.
        let named: Vec<KeyName> = Key::ALL.iter().filter_map(|key| key_name(*key)).collect();
        for binding in BINDINGS {
            for chord in binding.keys {
                assert!(
                    named.contains(&chord.key),
                    "{:?} has no egui key",
                    chord.key
                );
            }
        }
    }

    #[test]
    fn modifiers_must_match_exactly_and_typing_keeps_plain_keys() {
        let ctx = egui::Context::default();
        assert_eq!(
            press(&ctx, NONE, Key::S, false),
            Some(Action::SplitSelected)
        );
        assert_eq!(press(&ctx, CMD, Key::S, false), Some(Action::SaveProject));
        assert_eq!(
            press(&ctx, CMD_SHIFT, Key::S, false),
            Some(Action::SaveProjectAs)
        );
        assert_eq!(
            press(&ctx, SHIFT, Key::S, false),
            None,
            "Shift+S is not split"
        );
        assert_eq!(
            press(&ctx, SHIFT, Key::ArrowLeft, false),
            Some(Action::StepBackSecond)
        );
        assert_eq!(
            press(&ctx, CMD_SHIFT, Key::Plus, false),
            Some(Action::ZoomIn)
        );
        // fframes' bracket keys reach the same cut navigation as the arrows.
        assert_eq!(
            press(&ctx, NONE, Key::OpenBracket, false),
            Some(Action::PrevEdit)
        );
        assert_eq!(
            press(&ctx, NONE, Key::CloseBracket, false),
            Some(Action::NextEdit)
        );
        // A focused text field owns letters, arrows and its own undo.
        assert_eq!(press(&ctx, NONE, Key::S, true), None);
        assert_eq!(press(&ctx, NONE, Key::Space, true), None);
        assert_eq!(press(&ctx, CMD, Key::Z, true), None);
        assert_eq!(press(&ctx, CMD, Key::S, true), Some(Action::SaveProject));
        assert_eq!(
            press(&ctx, CMD, Key::Comma, true),
            Some(Action::OpenSettings)
        );
    }
}

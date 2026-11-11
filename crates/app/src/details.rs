//! The Details panel: the selected clip's name, timing, look and sound.
//! The edits themselves are `eddie_core::editor::details`; this module draws
//! the fields and turns typing, drags and presses into those calls.
//!
//! `fields` is the typed values and when typing reaches the clip, `body`
//! the cards for one clip, `rows` the controls those cards are made of and
//! `styles` the two title-style lists.

mod body;
mod fields;
mod rows;
mod styles;

use openreel_engine::ClipId;
use rune::{Cursor, El, Phase, ScrollSpec, TextEdit, col, el, row, text};
use rune_kit::{ScrollBar, ScrollBarStyle};

pub use fields::sync;
use fields::{Entry, Field};
pub use styles::{Select, overlays};

use crate::app::Ui;
use crate::theme::{self, c};

pub const DEFAULT_W: f32 = 340.0;
const MIN_W: f32 = 280.0;
const MAX_W: f32 = 460.0;
/// The panel's side padding; the left border is drawn inside it.
const PAD_X: f32 = 14.0;
/// A card's padding, its border included.
const CARD_PAD: f32 = 13.0;
const GAP: f32 = 8.0;
const LABEL_W: f32 = 78.0;
const BUTTON_GAP: f32 = 6.0;

pub struct State {
    /// The clip the fields were last filled from.
    pub shown: Option<ClipId>,
    pub name: TextEdit,
    entries: [Entry; 7],
    /// The value field that had the keyboard at the last poll.
    focused: Option<Field>,
    pub select: Option<Select>,
    /// A slider is held, so undo waits for the release.
    pub slider_held: bool,
    pub width: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            shown: None,
            name: TextEdit::new(""),
            entries: std::array::from_fn(|_| Entry {
                edit: TextEdit::new(""),
                filled: String::new(),
            }),
            focused: None,
            select: None,
            slider_held: false,
            width: DEFAULT_W,
        }
    }
}

impl State {
    /// A field here has the keyboard, so plain keys are text.
    pub fn typing(&self) -> bool {
        self.name.focused || self.entries.iter().any(|entry| entry.edit.focused)
    }

    fn entry(&self, field: Field) -> &Entry {
        &self.entries[field as usize]
    }

    fn entry_mut(&mut self, field: Field) -> &mut Entry {
        &mut self.entries[field as usize]
    }

    fn blur(&mut self) {
        self.name.focused = false;
        for entry in &mut self.entries {
            entry.edit.focused = false;
        }
    }
}

/// Export draws a title on one line.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

pub fn panel(ui: &Ui) -> El {
    let c = c();
    let width = ui.details.width;
    let mut content = col()
        .w_full()
        .h_full()
        .px(PAD_X)
        .py(12.0)
        .child(theme::panel_title("Details"))
        .child(el().h(18.0));
    let clip = ui
        .details
        .shown
        .and_then(|id| Some((id, ui.editor.timeline.clip(id)?)));
    content = match clip {
        Some((id, clip)) => content.child(
            col()
                .key("details-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(body::body(ui, id, clip))
                .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside()),
        ),
        None => content
            .child(el().h(24.0))
            .child(
                row().key("details-empty").w_full().center().child(
                    text("No clip selected".to_owned())
                        .font(theme::BODY)
                        .color(c.muted_fg)
                        .single_line(),
                ),
            )
            .child(el().h(5.0))
            .child(
                row()
                    .w_full()
                    .center()
                    .child(theme::caption("Click a clip on the timeline to edit it.")),
            ),
    };
    el().key("details")
        .w(width)
        .h_full()
        .fill(c.surface)
        .child(content)
        .child(el().absolute(0.0, 0.0).w(1.0).h_full().fill(c.border))
        .child(
            el().key("details-resize")
                .absolute(-3.0, 0.0)
                .w(7.0)
                .h_full()
                .cursor(Cursor::ResizeColumn)
                .help("Drag to resize Details")
                .on_drag(|ui: &mut Ui, drag, _| {
                    if drag.phase == Phase::Down {
                        drag.data = ui.details.width;
                    } else {
                        ui.details.width = (drag.data - drag.dx).round().clamp(MIN_W, MAX_W);
                    }
                }),
        )
}

#[cfg(test)]
mod tests;

//! The controls Details' cards are made of: a card, a labelled row, a typed
//! value, a slider, a switch and a row of buttons.

use eddie_core::Editor;
use openreel_engine::{Clip, ClipId};
use rune::{Align, Cursor, El, Key, Phase, el, row, text};

use super::fields::{Field, commit, revert};
use super::{BUTTON_GAP, CARD_PAD, GAP, LABEL_W};
use crate::app::Ui;
use crate::theme::{self, FIELD_H, c};

/// Under a card's last row.
const CARD_PAD_BOTTOM: f32 = 12.0;

/// A card title's line, in whole pixels so the rows under it stay sharp.
const CARD_TITLE_H: f32 = 17.0;

const ROW_H: f32 = 30.0;

const HANDLE: f32 = 24.0;

/// The rail starts this far inside the slider's left end,
const RAIL_INSET: f32 = 4.0;

/// and stops this far short of its right end.
const RAIL_END: f32 = 2.0;

/// The handle's centre stays this far inside the rail's ends.
const HANDLE_REACH: f32 = 11.2;

const PERCENT_W: f32 = 58.0;

/// A text button's height in whole pixels, so its edges stay sharp.
const BUTTON_H: f32 = 27.0;

pub(super) fn section(title: &str, rows: Vec<El>) -> El {
    theme::card()
        .padding(CARD_PAD)
        .pb(CARD_PAD_BOTTOM)
        .w_full()
        .gap(GAP)
        .child(
            text(title.to_owned())
                .font(theme::SEMIBOLD)
                .color(c().foreground)
                .single_line()
                .h(CARD_TITLE_H),
        )
        .children(rows)
}

/// A label and its control on one line.
pub(super) fn prop_row(label: &str, help: &str, control: El) -> El {
    row()
        .w_full()
        .h(ROW_H)
        .gap(GAP)
        .center_y()
        .help(help.to_owned())
        .child(
            text(label.to_owned())
                .font(theme::BODY)
                .color(c().muted_fg)
                .single_line()
                .w(LABEL_W),
        )
        .child(control)
}

/// The tooltip of a seconds field: what it is, and the value in full.
pub(super) fn seconds_help(what: &str, seconds: f64) -> String {
    format!("{what}\n{seconds:.3} s")
}

pub(super) fn seconds_row(ui: &Ui, label: &str, field: Field, clip: &Clip, what: &str) -> El {
    let help = seconds_help(what, field.value(clip));
    prop_row(
        label,
        &help,
        value_field(ui, field, help.clone()).grow(1.0).min_w(0.0),
    )
}

/// A typed value. Enter or leaving the field applies it; Esc does not.
pub(super) fn value_field(ui: &Ui, field: Field, help: String) -> El {
    let c = c();
    rune::text_input(&ui.details.entry(field).edit, theme::BODY)
        .selection_color(theme::selection())
        .key(field.key())
        .single_line()
        .align(Align::Center)
        .h(FIELD_H)
        .px(8.0)
        .py(6.0)
        .radius(theme::R_MD)
        .fill(theme::field_fill())
        .color(c.foreground)
        .focus_within_ring(1.0, -1.0, c.brand)
        .help(help)
        .on_edit(rune::edit_access(move |ui: &mut Ui| {
            &mut ui.details.entry_mut(field).edit
        }))
        .on_submit(move |ui: &mut Ui, tx| {
            commit(ui, field);
            tx.blur();
        })
        .on_input_key(move |ui: &mut Ui, event, tx| {
            if matches!(event.key, Key::Escape) {
                revert(ui, field);
                tx.blur();
                true
            } else {
                false
            }
        })
}

/// A 0-to-`max` slider with the same value as a typed percentage beside it.
#[allow(clippy::too_many_arguments)]
pub(super) fn slider_row(
    ui: &Ui,
    id: ClipId,
    label: &'static str,
    field: Field,
    (value, max): (f32, f32),
    value_w: f32,
    set: fn(&mut Editor, ClipId, f32),
    help: &'static str,
) -> El {
    let c = c();
    let width = value_w - BUTTON_GAP - PERCENT_W;
    let fraction = (value / max).clamp(0.0, 1.0);
    let (low, high) = handle_range(width);
    let x = low + fraction * (high - low);
    let rail = (HANDLE - 8.0) / 2.0;
    let slider = el()
        .key(format!("{}-slider", field.key()))
        .size(width, HANDLE)
        .mb(2.0)
        .child(
            el().absolute(RAIL_INSET, rail)
                .size(width - RAIL_INSET - RAIL_END, 8.0)
                .radius(4.0)
                .fill(c.secondary),
        )
        .child(
            // BRAND at 27% over the rail.
            el().absolute(RAIL_INSET, rail)
                .size((x - RAIL_INSET).max(0.0), 8.0)
                .radius(4.0)
                .fill(theme::hex("#2C4062")),
        )
        .child(
            el().absolute(x - HANDLE / 2.0, 0.0)
                .size(HANDLE, HANDLE)
                .radius(HANDLE / 2.0)
                .fill(c.secondary)
                .border(1.0, theme::hex("#E2E2E2")),
        )
        .instant_changes()
        .cursor(Cursor::Pointer)
        .help(help)
        .label(label)
        .range_value(f64::from(value), 0.0, f64::from(max))
        .on_step(move |ui: &mut Ui, steps: i32, _| {
            set(&mut ui.editor, id, value + steps as f32 * 0.01);
        })
        .on_drag(move |ui: &mut Ui, drag, _| {
            ui.details.slider_held = drag.phase != Phase::Up;
            let (low, high) = handle_range(drag.w);
            if drag.phase == Phase::Up || high <= low {
                return;
            }
            let fraction = ((drag.x - low) / (high - low)).clamp(0.0, 1.0);
            // Whole percents, as the field beside it shows.
            set(&mut ui.editor, id, (fraction * max * 100.0).round() / 100.0);
        });
    prop_row(
        label,
        help,
        row()
            .grow(1.0)
            .min_w(0.0)
            .gap(BUTTON_GAP)
            .center_y()
            .child(slider)
            .child(value_field(ui, field, help.to_owned()).w(PERCENT_W)),
    )
}

/// The last slider row before a shorter row sits one pixel closer to it.
pub(super) fn last_slider(row: El) -> El {
    row.h(ROW_H - 1.0).pt(1.0)
}

/// Where the handle's centre runs along a slider `width` wide.
fn handle_range(width: f32) -> (f32, f32) {
    (RAIL_INSET + HANDLE_REACH, width - RAIL_END - HANDLE_REACH)
}

/// A switch on the clip itself.
pub(super) fn checkbox(
    id: ClipId,
    key: &'static str,
    label: &'static str,
    help: &'static str,
    on: bool,
    toggle: fn(&mut Clip),
) -> El {
    let control = theme::checkbox(key, label, help, on).on_press(move |ui: &mut Ui| {
        if let Some(clip) = ui.editor.timeline.clip_mut(id) {
            toggle(clip);
        }
    });
    // The row keeps the press to the box and its label.
    row().w_full().child(control)
}

/// Buttons sharing `width` equally.
pub(super) fn button_row(buttons: Vec<El>, width: f32) -> El {
    let count = buttons.len().max(1) as f32;
    let each = ((width - BUTTON_GAP * (count - 1.0)) / count).floor();
    row()
        .w_full()
        .gap(BUTTON_GAP)
        .children(buttons.into_iter().map(|button| button.w(each).h(BUTTON_H)))
}

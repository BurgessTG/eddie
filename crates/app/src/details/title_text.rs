//! The Text card: a title's size, colour, box, outline and place over its
//! style. The clamps are `Editor::edit_title_text`'s; the monitor, export
//! and contact sheets all draw from `openreel_media::clip_title_look`.

use eddie_core::Editor;
use openreel_engine::{Clip, ClipId, MAX_OUTLINE, MAX_TEXT_SIZE, MIN_TEXT_SIZE, Rgb, TextLook};
use rune::{Color, Cursor, El, LineCap, Shape, el, row};

use super::BUTTON_GAP;
use super::fields::{Field, set_colour};
use super::rows::{button_row, prop_row, section, slider_row, value_field};
use crate::app::Ui;
use crate::theme::{self, Variant, c};

/// The colours a swatch row offers, as CapCut's text palette starts.
pub(super) const PALETTE: [(&str, Rgb); 10] = [
    ("White", Rgb(255, 255, 255)),
    ("Black", Rgb(0, 0, 0)),
    ("Yellow", Rgb(255, 214, 0)),
    ("Orange", Rgb(255, 138, 0)),
    ("Red", Rgb(255, 59, 48)),
    ("Pink", Rgb(255, 79, 163)),
    ("Purple", Rgb(168, 85, 247)),
    ("Blue", Rgb(59, 130, 246)),
    ("Cyan", Rgb(34, 211, 238)),
    ("Green", Rgb(34, 197, 94)),
];

pub(super) const SWATCH_GAP: f32 = 6.0;

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let drawn = openreel_media::drawn_text(clip);
    let box_on = drawn.box_opacity.unwrap_or(0.0) > 0.0;
    let outline_on = drawn.outline.unwrap_or(0.0) > 0.0;
    section(
        "Text",
        vec![
            slider_row(
                ui,
                id,
                "Size",
                Field::TextSize,
                clip.text.size.log2() as f32,
                MIN_TEXT_SIZE.log2() as f32..=MAX_TEXT_SIZE.log2() as f32,
                value_w,
                size_at,
                "Size of the text against its style's; it grows upward",
            ),
            colour_row(ui, Field::TextColor, "Colour", "The colour of the letters"),
            swatches(id, Field::TextColor, drawn.color, None, card_inner),
            colour_row(
                ui,
                Field::BoxColor,
                "Box",
                "The colour of the box behind the text",
            ),
            swatches(
                id,
                Field::BoxColor,
                drawn.box_color,
                Some(box_on),
                card_inner,
            ),
            slider_row(
                ui,
                id,
                "Box opacity",
                Field::BoxOpacity,
                drawn.box_opacity.unwrap_or(0.0) as f32,
                0.0..=1.0,
                value_w,
                |e, id, v| e.edit_title_text(id, |t| t.box_opacity = Some(f64::from(v))),
                "How solid the box is; 0% draws none",
            ),
            colour_row(
                ui,
                Field::OutlineColor,
                "Outline",
                "The colour of the line round each letter",
            ),
            swatches(
                id,
                Field::OutlineColor,
                drawn.outline_color,
                Some(outline_on),
                card_inner,
            ),
            slider_row(
                ui,
                id,
                "Thickness",
                Field::Outline,
                drawn.outline.unwrap_or(0.0) as f32,
                0.0..=MAX_OUTLINE as f32,
                value_w,
                |e, id, v| e.edit_title_text(id, |t| t.outline = Some(f64::from(v))),
                "How thick the outline is, as a share of the text size; 0% draws none",
            ),
            position(ui, value_w),
            reset(id, card_inner),
        ],
    )
}

/// The Size slider's handle at `position`, log2 of the size, in the
/// hundredths the field shows. A step too small to change them still moves
/// one hundredth, so the arrow keys never stick at the small end.
fn size_at(editor: &mut Editor, id: ClipId, position: f32) {
    let Some(current) = editor.timeline.clip(id).map(|clip| clip.text.size) else {
        return;
    };
    let wanted = 2f64.powf(f64::from(position));
    let mut size = (wanted * 100.0).round() / 100.0;
    if size == current && wanted != current {
        size += if wanted > current { 0.01 } else { -0.01 };
    }
    editor.edit_title_text(id, |t| t.size = size);
}

/// A labelled `#RRGGBB` field.
fn colour_row(ui: &Ui, field: Field, label: &str, help: &str) -> El {
    let help = format!("{help}. Type any colour as #RRGGBB, or pick one below");
    prop_row(
        label,
        &help,
        value_field(ui, field, help.clone()).grow(1.0).min_w(0.0),
    )
}

/// A row of colours to pick for `field`, the one `current` shows ringed.
/// `on` is whether a box or outline is drawn; it adds a None swatch first.
fn swatches(id: ClipId, field: Field, current: Option<Rgb>, on: Option<bool>, width: f32) -> El {
    // One size in every row: the rows with a None swatch first set it.
    let most = PALETTE.len() + 1;
    let side = ((width - SWATCH_GAP * (most - 1) as f32) / most as f32)
        .floor()
        .min(24.0);
    let count = PALETTE.len() + usize::from(on.is_some());
    let what = match field {
        Field::BoxColor => "box",
        Field::OutlineColor => "outline",
        _ => "text",
    };
    let mut swatches = Vec::with_capacity(count);
    if let Some(on) = on {
        swatches.push(
            swatch(
                format!("{}-none", field_key(field)),
                side,
                None,
                !on,
                &format!("No {what}"),
            )
            .on_press(move |ui: &mut Ui| {
                ui.editor.edit_title_text(id, |t| match field {
                    Field::BoxColor => t.box_opacity = Some(0.0),
                    _ => t.outline = Some(0.0),
                });
            }),
        );
    }
    for (name, colour) in PALETTE {
        let current = on != Some(false) && current == Some(colour);
        swatches.push(
            swatch(
                format!("{}-{}", field_key(field), name.to_lowercase()),
                side,
                Some(colour),
                current,
                &format!("{name} {what} ({})", colour.hex()),
            )
            .on_press(move |ui: &mut Ui| set_colour(&mut ui.editor, id, field, colour)),
        );
    }
    row().w_full().gap(SWATCH_GAP).py(2.0).children(swatches)
}

fn field_key(field: Field) -> &'static str {
    match field {
        Field::BoxColor => "details-box",
        Field::OutlineColor => "details-outline",
        _ => "details-text",
    }
}

/// One colour to pick, or none (a slashed empty square).
pub(super) fn swatch(key: String, side: f32, colour: Option<Rgb>, current: bool, help: &str) -> El {
    let c = c();
    let mut swatch = el()
        .key(key)
        .size(side, side)
        .radius(theme::R_SM)
        .border(1.0, Color::WHITE.alpha(0.16))
        .cursor(Cursor::Pointer)
        .role("option")
        .label(help.to_owned())
        .help(format!("Use {}", help.to_lowercase()));
    swatch = match colour {
        Some(colour) => swatch.fill(rune::color::hex(&colour.hex()).expect("a colour from bytes")),
        None => swatch.shape(Shape::new().view_box(side, side).line(
            [4.0, side - 4.0],
            [side - 4.0, 4.0],
            1.5,
            LineCap::Round,
            c.destructive,
        )),
    };
    if current {
        // Ringed apart from its colour, so the blue swatch shows it too.
        swatch.outline(2.0, 1.5, c.brand)
    } else {
        swatch.hover(c.secondary)
    }
}

fn position(ui: &Ui, value_w: f32) -> El {
    let half = (value_w - BUTTON_GAP) / 2.0;
    prop_row(
        "Position",
        "Move the text from its style's place, in percent of the canvas: right and down are positive. Or drag it on the monitor",
        row()
            .grow(1.0)
            .min_w(0.0)
            .gap(BUTTON_GAP)
            .child(
                value_field(
                    ui,
                    Field::TextX,
                    "Across: percent of the canvas width, right is positive".to_owned(),
                )
                .w(half),
            )
            .child(
                value_field(
                    ui,
                    Field::TextY,
                    "Down: percent of the canvas height, down is positive".to_owned(),
                )
                .w(half),
            ),
    )
}

fn reset(id: ClipId, card_inner: f32) -> El {
    button_row(
        vec![
            theme::button(
                "details-text-reset",
                "Reset",
                Variant::Secondary,
                "Draw the text as its style does: size, colours, box, outline and place",
                true,
            )
            .on_press(move |ui: &mut Ui| {
                ui.editor.edit_title_text(id, |t| *t = TextLook::default());
            }),
        ],
        card_inner,
    )
    .py(2.0)
}

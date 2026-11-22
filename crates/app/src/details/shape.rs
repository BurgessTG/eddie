//! The Shape card: a shape clip's kind, picked from tiles, its fill and
//! outline colours, the outline's thickness, rounded corners and
//! proportions. Each change draws the shape again
//! (`Editor::edit_clip_shape`), so slider values are rounded to steps: a
//! drag draws once a step, not once a frame.

use eddie_core::Editor;
use openreel_engine::{
    Clip, ClipId, Graphic, MAX_SHAPE_OUTLINE, MAX_SHAPE_RATIO, MIN_SHAPE_OUTLINE, Rgb, Shape,
    ShapeKind,
};
use rune::{El, row};

use super::fields::Field;
use super::filters::{tile, tile_grid, tile_size};
use super::rows::{button_row, prop_row, section, slider_row, value_field};
use super::title_text::{PALETTE, SWATCH_GAP, swatch};
use crate::app::Ui;
use crate::media::graphics::{pixels, plain};
use crate::theme::{self, Variant};

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let Some(Graphic::Shape(shape)) = clip.graphic else {
        return section("Shape", Vec::new());
    };
    let (cols, w, h) = tile_size(card_inner);
    let tiles = ShapeKind::ALL
        .into_iter()
        .map(|kind| {
            let mut picture = rune::el().absolute(0.0, 0.0).size(w, h).padding(8.0);
            let long = ((w.max(h) - 16.0) * 2.0).round() as u32;
            if let Some(pixels) = pixels(&plain(kind), long) {
                picture = picture.child(
                    rune::el()
                        .size(w - 16.0, h - 16.0)
                        .pixels_fit(&pixels, rune::SurfaceFit::Fit),
                );
            }
            tile(
                format!("details-shape-{}", kind.as_str()),
                // "Speech bubble" does not fit a tile; its tooltip says it.
                match kind {
                    ShapeKind::Bubble => "Bubble",
                    _ => kind.label(),
                },
                help(kind),
                picture,
                shape.kind == kind,
                (w, h),
            )
            .on_press(move |ui: &mut Ui| {
                ui.editor
                    .edit_clip_shape(id, |shape| *shape = shape.with_kind(kind));
            })
        })
        .collect();
    let mut rows = vec![
        tile_grid("details-shapes", tiles, cols),
        colour_row(ui, Field::ShapeFill, "Fill", "The colour inside the shape"),
        swatches(id, Field::ShapeFill, shape.fill, card_inner),
        colour_row(
            ui,
            Field::ShapeOutline,
            "Outline",
            "The colour of the shape's edge",
        ),
        swatches(id, Field::ShapeOutline, shape.outline, card_inner),
    ];
    if shape.outline.is_some() {
        rows.push(slider_row(
            ui,
            id,
            "Thickness",
            Field::ShapeThickness,
            shape.thickness as f32,
            MIN_SHAPE_OUTLINE as f32..=MAX_SHAPE_OUTLINE as f32,
            value_w,
            // Half-percent steps.
            |e, id, v| e.edit_clip_shape(id, |s| s.thickness = (f64::from(v) * 200.0).round() / 200.0),
            "How thick the outline is, in percent of the shape's shorter side; it lies inside the edge",
        ));
    }
    if shape.kind.has_round() {
        rows.push(slider_row(
            ui,
            id,
            "Round",
            Field::ShapeRound,
            shape.round as f32,
            0.0..=1.0,
            value_w,
            |e, id, v| e.edit_clip_shape(id, |s| s.round = (f64::from(v) * 100.0).round() / 100.0),
            "How round the corners are; 100% makes the short sides half circles",
        ));
    }
    // Rounded up, so the slider's ends (in hundredths) reach the range's.
    let widest = ((MAX_SHAPE_RATIO.log2() * 100.0).ceil() / 100.0) as f32;
    rows.push(slider_row(
        ui,
        id,
        "Ratio",
        Field::ShapeRatio,
        shape.ratio.log2() as f32,
        -widest..=widest,
        value_w,
        ratio_at,
        "Width over height: 2 is twice as wide as tall, 0.5 twice as tall as wide. Scale it in Transform",
    ));
    rows.push(
        button_row(
            vec![
                theme::button(
                    "details-shape-reset",
                    "Reset",
                    Variant::Secondary,
                    "Draw the shape plain white at its own proportions, without an outline",
                    true,
                )
                .on_press(move |ui: &mut Ui| {
                    ui.editor
                        .edit_clip_shape(id, |shape| *shape = Shape::new(shape.kind));
                }),
            ],
            card_inner,
        )
        .py(2.0),
    );
    section("Shape", rows)
}

/// The Ratio slider's handle at `position`, log2 of the ratio, in the
/// hundredths the field shows. A step too small to change them still moves
/// one hundredth, so the arrow keys never stick at the narrow end.
fn ratio_at(editor: &mut Editor, id: ClipId, position: f32) {
    let Some(Some(Graphic::Shape(current))) =
        editor.timeline.clip(id).map(|clip| clip.graphic.clone())
    else {
        return;
    };
    let wanted = 2f64.powf(f64::from(position));
    let mut ratio = (wanted * 100.0).round() / 100.0;
    if ratio == current.ratio && wanted != current.ratio {
        ratio += if wanted > current.ratio { 0.01 } else { -0.01 };
    }
    editor.edit_clip_shape(id, |s| s.ratio = ratio);
}

/// A labelled `#RRGGBB` field that also takes "None".
fn colour_row(ui: &Ui, field: Field, label: &str, help: &str) -> El {
    let help = format!("{help}. Type any colour as #RRGGBB or None, or pick one below");
    prop_row(
        label,
        &help,
        value_field(ui, field, help.clone()).grow(1.0).min_w(0.0),
    )
}

/// None, then the palette, for the fill or the outline; `current` ringed.
fn swatches(id: ClipId, field: Field, current: Option<Rgb>, width: f32) -> El {
    let count = PALETTE.len() + 1;
    let side = ((width - SWATCH_GAP * (count - 1) as f32) / count as f32)
        .floor()
        .min(24.0);
    let (key, what) = match field {
        Field::ShapeFill => ("details-shape-fill", "fill"),
        _ => ("details-shape-outline", "outline"),
    };
    let set = move |colour: Option<Rgb>| {
        move |ui: &mut Ui| {
            ui.editor.edit_clip_shape(id, |shape| match field {
                Field::ShapeFill => shape.fill = colour,
                _ => shape.outline = colour,
            });
        }
    };
    let mut swatches = vec![
        swatch(
            format!("{key}-none"),
            side,
            None,
            current.is_none(),
            &format!("No {what}"),
        )
        .on_press(set(None)),
    ];
    for (name, colour) in PALETTE {
        swatches.push(
            swatch(
                format!("{key}-{}", name.to_lowercase()),
                side,
                Some(colour),
                current == Some(colour),
                &format!("{name} {what} ({})", colour.hex()),
            )
            .on_press(set(Some(colour))),
        );
    }
    row().w_full().gap(SWATCH_GAP).py(2.0).children(swatches)
}

/// What each kind draws, for its tooltip.
fn help(kind: ShapeKind) -> &'static str {
    match kind {
        ShapeKind::Rectangle => "Rectangle: its corners rounded if you like",
        ShapeKind::Circle => "Circle: a circle or an oval",
        ShapeKind::Triangle => "Triangle: point up",
        ShapeKind::Star => "Star: five points",
        ShapeKind::Heart => "Heart",
        ShapeKind::Arrow => "Arrow: pointing right; turn it in Transform",
        ShapeKind::Bubble => "Speech bubble: its tail at the lower left",
    }
}

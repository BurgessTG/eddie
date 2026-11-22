//! The Mask card: a shape on a video clip's picture that keeps what is
//! inside it and clears the rest, picked from tiles that draw each shape
//! with the coverage the renderers use, then placed, sized, turned,
//! feathered and inverted. The clamps are `Editor::edit_clip_mask`'s.

use std::cell::RefCell;

use openreel_engine::{Clip, ClipId, Mask, MaskShape};
use rune::{El, LineCap, Pixels, Shape, SurfaceFit, el, row};

use super::BUTTON_GAP;
use super::fields::Field;
use super::filters::{tile, tile_grid, tile_size};
use super::rows::{button_row, checkbox, prop_row, section, slider_row, value_field};
use crate::app::Ui;
use crate::theme::{self, Variant, c};

/// The Width and Height sliders' right end; the fields take up to 500%.
const SIZE_SLIDER_MAX: f32 = 2.0;

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let current = clip.mask;
    let (cols, w, h) = tile_size(card_inner);
    let none = el()
        .absolute(0.0, 0.0)
        .size(w, h)
        .shape(Shape::new().view_box(w, h).line(
            [w * 0.3, h * 0.75],
            [w * 0.7, h * 0.25],
            1.5,
            LineCap::Round,
            c().muted_fg,
        ));
    let mut tiles = vec![
        tile(
            "details-mask-none".to_owned(),
            "None",
            "No mask: the whole picture shows",
            none,
            current.is_none(),
            (w, h),
        )
        .on_press(move |ui: &mut Ui| ui.editor.edit_clip_mask(id, |mask| *mask = None)),
    ];
    let pictures = shape_pictures((w, h));
    for (shape, pixels) in MaskShape::ALL.into_iter().zip(pictures) {
        let picture = el()
            .absolute(0.0, 0.0)
            .size(w, h)
            .pixels_fit(&pixels, SurfaceFit::Fill);
        tiles.push(
            tile(
                format!("details-mask-{}", shape.as_str()),
                shape.label(),
                help(shape),
                picture,
                current.is_some_and(|mask| mask.shape == shape),
                (w, h),
            )
            .on_press(move |ui: &mut Ui| {
                ui.editor.edit_clip_mask(id, |mask| match mask {
                    Some(mask) => mask.shape = shape,
                    None => *mask = Some(Mask::new(shape)),
                });
            }),
        );
    }
    let mut rows = vec![tile_grid("details-masks", tiles, cols)];
    let Some(mask) = current else {
        return section("Mask", rows);
    };
    rows.push(position(ui, value_w));
    if mask.shape.has_width() {
        rows.push(slider_row(
            ui,
            id,
            "Width",
            Field::MaskWidth,
            mask.width as f32,
            0.0..=SIZE_SLIDER_MAX,
            value_w,
            |e, id, v| e.edit_clip_mask(id, |m| m.get_or_insert_default().width = f64::from(v)),
            "How wide the shape is, in percent of the picture's shorter side",
        ));
    }
    if mask.shape.has_height() {
        rows.push(slider_row(
            ui,
            id,
            "Height",
            Field::MaskHeight,
            mask.height as f32,
            0.0..=SIZE_SLIDER_MAX,
            value_w,
            |e, id, v| e.edit_clip_mask(id, |m| m.get_or_insert_default().height = f64::from(v)),
            if mask.shape == MaskShape::Filmstrip {
                "How tall the band is, in percent of the picture's shorter side"
            } else {
                "How tall the shape is, in percent of the picture's shorter side"
            },
        ));
    }
    rows.push(slider_row(
        ui,
        id,
        "Rotation",
        Field::MaskRotation,
        mask.rotation as f32,
        -180.0..=180.0,
        value_w,
        |e, id, v| e.edit_clip_mask(id, |m| m.get_or_insert_default().rotation = f64::from(v)),
        "Turn the shape clockwise on the picture, in degrees",
    ));
    rows.push(slider_row(
        ui,
        id,
        "Feather",
        Field::MaskFeather,
        mask.feather as f32,
        0.0..=1.0,
        value_w,
        |e, id, v| e.edit_clip_mask(id, |m| m.get_or_insert_default().feather = f64::from(v)),
        "How soft the shape's edge is; 100% fades over half the picture's shorter side",
    ));
    if mask.shape == MaskShape::Rectangle {
        rows.push(slider_row(
            ui,
            id,
            "Round",
            Field::MaskRound,
            mask.round as f32,
            0.0..=1.0,
            value_w,
            |e, id, v| e.edit_clip_mask(id, |m| m.get_or_insert_default().round = f64::from(v)),
            "How round the rectangle's corners are; 100% makes its short sides half circles",
        ));
    }
    rows.push(checkbox(
        id,
        "details-mask-invert",
        "Invert",
        "Keep the picture outside the shape and clear the inside",
        mask.invert,
        |clip| {
            if let Some(mask) = &mut clip.mask {
                mask.invert = !mask.invert;
            }
        },
    ));
    rows.push(
        button_row(
            vec![
                theme::button(
                    "details-mask-reset",
                    "Reset",
                    Variant::Secondary,
                    "Put the shape back in the middle at its first size, unturned and hard-edged",
                    true,
                )
                .on_press(move |ui: &mut Ui| {
                    ui.editor.edit_clip_mask(id, |mask| {
                        if let Some(mask) = mask {
                            *mask = Mask::new(mask.shape);
                        }
                    });
                }),
            ],
            card_inner,
        )
        .py(2.0),
    );
    section("Mask", rows)
}

fn position(ui: &Ui, value_w: f32) -> El {
    let half = (value_w - BUTTON_GAP) / 2.0;
    prop_row(
        "Position",
        "Move the shape on the picture, in percent of its width and height: right and down are positive",
        row()
            .grow(1.0)
            .min_w(0.0)
            .gap(BUTTON_GAP)
            .child(
                value_field(
                    ui,
                    Field::MaskX,
                    "Across: percent of the picture's width, right is positive".to_owned(),
                )
                .w(half),
            )
            .child(
                value_field(
                    ui,
                    Field::MaskY,
                    "Down: percent of the picture's height, down is positive".to_owned(),
                )
                .w(half),
            ),
    )
}

/// The tile pictures made last, and the size in pixels they were made at.
type Made = Option<((u32, u32), Vec<Pixels>)>;

/// Each shape drawn light on a tile `w` by `h` points, at twice that in
/// pixels, from the same coverage the renderers use. Made once per tile
/// size.
fn shape_pictures((w, h): (f32, f32)) -> Vec<Pixels> {
    thread_local! {
        static MADE: RefCell<Made> = const { RefCell::new(None) };
    }
    let size = ((w * 2.0).round() as u32, (h * 2.0).round() as u32);
    MADE.with_borrow_mut(|made| {
        if let Some((_, pictures)) = made.as_ref().filter(|(at, _)| *at == size) {
            return pictures.clone();
        }
        let (pw, ph) = size;
        let ink = c().foreground;
        let rgb = [ink.r, ink.g, ink.b]
            .map(|v| (rune::color::srgb_encode(v).clamp(0.0, 1.0) * 255.0).round() as u8);
        let pictures: Vec<Pixels> = MaskShape::ALL
            .into_iter()
            .map(|shape| {
                let levels = Mask::new(shape).on(f64::from(pw), f64::from(ph)).levels(
                    pw as usize,
                    ph as usize,
                    (0.0, 0.0),
                );
                let rgba = levels
                    .iter()
                    .flat_map(|&level| {
                        [rgb[0], rgb[1], rgb[2], (u32::from(level) * 217 / 255) as u8]
                    })
                    .collect();
                let pixels = Pixels::new();
                pixels.set(pw, ph, rgba);
                pixels
            })
            .collect();
        *made = Some((size, pictures.clone()));
        pictures
    })
}

/// What each shape keeps, for its tooltip.
fn help(shape: MaskShape) -> &'static str {
    match shape {
        MaskShape::Split => "Split: keep the picture on one side of a straight line",
        MaskShape::Filmstrip => "Filmstrip: keep a band across the picture",
        MaskShape::Circle => "Circle: keep a circle or an oval",
        MaskShape::Rectangle => "Rectangle: keep a rectangle, its corners rounded if you like",
        MaskShape::Heart => "Heart: keep a heart",
        MaskShape::Star => "Star: keep a five-pointed star",
    }
}

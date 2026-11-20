//! The Transform card: where a video clip's picture sits on the canvas.
//! The clamps are `Editor::edit_clip_transform`'s; preview and export both
//! place the picture with `openreel_engine::place`.

use eddie_core::Editor;
use openreel_engine::{Background, Clip, ClipId, Property, Transform};
use rune::{El, row};

use super::BUTTON_GAP;
use super::fields::Field;
use super::keys::{self, KEY_SPACE};
use super::rows::{button_row, checkbox, prop_row, section, slider_row, value_field};
use crate::app::Ui;
use crate::theme::{self, Variant};

/// The Scale slider's right end; the field takes up to 2000%.
const SLIDER_MAX: f32 = 4.0;

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let t = clip.transform;
    let key = |properties, key, what| keys::button(ui, id, clip, properties, key, what);
    section(
        "Transform",
        vec![
            slider_row(
                ui,
                id,
                "Scale",
                Field::Scale,
                t.scale as f32,
                0.0..=SLIDER_MAX,
                value_w - KEY_SPACE,
                set_scale,
                "Size of the picture; 100% fits it inside the canvas",
            )
            .child(key(&[Property::Scale], "details-key-scale", "scale")),
            position(ui, value_w - KEY_SPACE).child(key(
                &[Property::X, Property::Y],
                "details-key-position",
                "position",
            )),
            rotation(ui, id, t, value_w - KEY_SPACE).child(key(
                &[Property::Rotation],
                "details-key-rotation",
                "rotation",
            )),
            crop(ui, value_w),
            checkbox(
                id,
                "details-flip-h",
                "Flip horizontally",
                "Mirror the picture left to right",
                t.flip_h,
                |clip| clip.transform.flip_h = !clip.transform.flip_h,
            ),
            checkbox(
                id,
                "details-flip-v",
                "Flip vertically",
                "Mirror the picture top to bottom",
                t.flip_v,
                |clip| clip.transform.flip_v = !clip.transform.flip_v,
            ),
            background(id, clip.background),
            fit_buttons(id, card_inner),
        ],
    )
}

fn set_scale(editor: &mut Editor, id: ClipId, scale: f32) {
    editor.edit_clip_transform(id, |t| t.scale = f64::from(scale));
}

fn position(ui: &Ui, value_w: f32) -> El {
    let half = (value_w - BUTTON_GAP) / 2.0;
    prop_row(
        "Position",
        "Move the picture, in percent of the canvas: right and down are positive",
        row()
            .grow(1.0)
            .min_w(0.0)
            .gap(BUTTON_GAP)
            .child(
                value_field(
                    ui,
                    Field::X,
                    "Across: percent of the canvas width, right is positive".to_owned(),
                )
                .w(half),
            )
            .child(
                value_field(
                    ui,
                    Field::Y,
                    "Down: percent of the canvas height, down is positive".to_owned(),
                )
                .w(half),
            ),
    )
}

fn rotation(ui: &Ui, id: ClipId, t: Transform, value_w: f32) -> El {
    let half = (value_w - BUTTON_GAP) / 2.0;
    prop_row(
        "Rotation",
        "Turn the picture clockwise, in degrees",
        row()
            .grow(1.0)
            .min_w(0.0)
            .gap(BUTTON_GAP)
            .center_y()
            .child(
                value_field(
                    ui,
                    Field::Rotation,
                    "Clockwise degrees; a keyed turn may pass 360 to keep turning".to_owned(),
                )
                .w(half),
            )
            .child(
                theme::button(
                    "details-rotate",
                    "Rotate 90°",
                    Variant::Secondary,
                    "Turn the picture a quarter clockwise",
                    true,
                )
                .w(half)
                .on_press(move |ui: &mut Ui| {
                    // A whole quarter on from the nearest one below.
                    let next = (t.rotation / 90.0).floor() * 90.0 + 90.0;
                    ui.editor.edit_clip_transform(id, |t| t.rotation = next);
                }),
            ),
    )
}

fn crop(ui: &Ui, value_w: f32) -> El {
    let quarter = (value_w - 3.0 * BUTTON_GAP) / 4.0;
    let sides = [
        (Field::CropLeft, "left"),
        (Field::CropTop, "top"),
        (Field::CropRight, "right"),
        (Field::CropBottom, "bottom"),
    ];
    prop_row(
        "Crop",
        "Cut a share of the picture from each edge: left, top, right, bottom",
        row()
            .grow(1.0)
            .min_w(0.0)
            .gap(BUTTON_GAP)
            .children(sides.into_iter().map(|(field, side)| {
                value_field(
                    ui,
                    field,
                    format!("Crop {side}: percent of the picture cut from its {side} edge"),
                )
                .w(quarter)
            })),
    )
}

fn background(id: ClipId, current: Background) -> El {
    let help = "What fills the canvas around the picture";
    let choices = Background::ALL;
    prop_row(
        "Background",
        help,
        theme::segmented(
            "details-background",
            choices
                .into_iter()
                .map(|background| {
                    (
                        format!("details-background-{}", background.as_str()),
                        match background {
                            Background::Black => "Black",
                            Background::Blur => "Blur",
                        },
                        match background {
                            Background::Black => "Black bars around the picture",
                            Background::Blur => {
                                "An enlarged, blurred copy of the picture fills the canvas around it"
                            }
                        },
                        background == current,
                    )
                })
                .collect(),
            move |index, segment| {
                let background = choices[index];
                segment.on_press(move |ui: &mut Ui| {
                    ui.editor.set_clip_background(id, background);
                })
            },
        ),
    )
}

fn fit_buttons(id: ClipId, card_inner: f32) -> El {
    button_row(
        vec![
            theme::button(
                "details-fit",
                "Fit",
                Variant::Secondary,
                "Show the whole picture inside the canvas, centred",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.editor.fit_clip(id, false)),
            theme::button(
                "details-fill",
                "Fill",
                Variant::Secondary,
                "Cover the whole canvas, centred, cutting off what spills over",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.editor.fit_clip(id, true)),
            theme::button(
                "details-transform-reset",
                "Reset",
                Variant::Secondary,
                "Put the picture back: fitted, centred, upright, unflipped and uncropped, without position, scale or rotation keys",
                true,
            )
            .on_press(move |ui: &mut Ui| {
                if let Some(clip) = ui.editor.timeline.clip_mut(id) {
                    for property in [Property::X, Property::Y, Property::Scale, Property::Rotation] {
                        clip.keyframes.clear(property);
                    }
                }
                ui.editor
                    .edit_clip_transform(id, |t| *t = Transform::default());
            }),
        ],
        card_inner,
    )
    .py(2.0)
}

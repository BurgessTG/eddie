//! The Chroma key card: a video clip's key colour, how much of the picture
//! near it clears, how soft the edge is and how much colour cast comes
//! off what stays. The clamps are `Editor::edit_clip_chroma`'s; the
//! preview keys its frames and export builds FFmpeg filters from the same
//! `openreel_engine::ChromaKey`. The colour can be picked on the monitor
//! (`crate::monitor::picture::pick_key`).

use openreel_engine::{ChromaKey, Clip, ClipId, KEY_BLUE, KEY_GREEN, Rgb};
use rune::El;

use super::fields::Field;
use super::rows::{button_row, checkbox, prop_row, section, slider_row, value_field};
use crate::app::Ui;
use crate::theme::{self, Variant};

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let mut rows = vec![checkbox(
        id,
        "details-chroma",
        "Key out a colour",
        "Clear the parts of the picture near one colour, such as a green screen, so the lanes under it show through",
        clip.chroma.is_some(),
        |clip| {
            clip.chroma = match clip.chroma {
                Some(_) => None,
                None => Some(ChromaKey::default()),
            }
        },
    )];
    let Some(key) = clip.chroma else {
        return section("Chroma key", rows);
    };
    let help = "The colour to clear. Type any colour as #RRGGBB, or pick it on the monitor";
    rows.push(prop_row(
        "Colour",
        help,
        value_field(ui, Field::KeyColor, help.to_owned())
            .grow(1.0)
            .min_w(0.0),
    ));
    let picking = ui.key_pick == Some(id);
    let preset = move |key_name: &'static str, label: &str, colour: Rgb, help: &str| {
        theme::button(key_name, label, Variant::Secondary, help, true).on_press(
            move |ui: &mut Ui| {
                ui.key_pick = None;
                ui.editor.edit_clip_chroma(id, |key| {
                    key.get_or_insert_default().color = colour;
                });
            },
        )
    };
    rows.push(
        button_row(
            vec![
                preset(
                    "details-chroma-green",
                    "Green",
                    KEY_GREEN,
                    "Key out a green screen (#00B140)",
                ),
                preset(
                    "details-chroma-blue",
                    "Blue",
                    KEY_BLUE,
                    "Key out a blue screen (#0047BB)",
                ),
                theme::button(
                    "details-chroma-pick",
                    if picking { "Click it…" } else { "Pick" },
                    if picking {
                        Variant::Primary
                    } else {
                        Variant::Secondary
                    },
                    if picking {
                        "Click the colour to clear on the monitor; press again to stop"
                    } else {
                        "Pick the colour to clear by clicking it on the monitor"
                    },
                    true,
                )
                .on_press(move |ui: &mut Ui| {
                    ui.key_pick = if ui.key_pick == Some(id) {
                        None
                    } else {
                        Some(id)
                    };
                }),
            ],
            card_inner,
        )
        .py(2.0),
    );
    rows.extend([
        slider_row(
            ui,
            id,
            "Strength",
            Field::KeyStrength,
            key.strength as f32,
            0.0..=1.0,
            value_w,
            |e, id, v| {
                e.edit_clip_chroma(id, |k| k.get_or_insert_default().strength = f64::from(v))
            },
            "How far from the colour still clears; raise it until the screen is gone. At 100% greys clear too",
        ),
        slider_row(
            ui,
            id,
            "Softness",
            Field::KeySoftness,
            key.softness as f32,
            0.0..=1.0,
            value_w,
            |e, id, v| {
                e.edit_clip_chroma(id, |k| k.get_or_insert_default().softness = f64::from(v))
            },
            "How gradual the edge between cleared and kept is; 0% cuts hard",
        ),
        slider_row(
            ui,
            id,
            "Spill",
            Field::KeySpill,
            key.spill as f32,
            0.0..=1.0,
            value_w,
            |e, id, v| e.edit_clip_chroma(id, |k| k.get_or_insert_default().spill = f64::from(v)),
            "Take the screen's colour cast off what stays, such as green on hair and skin",
        ),
    ]);
    section("Chroma key", rows)
}

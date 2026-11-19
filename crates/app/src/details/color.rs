//! The Colour card: a video clip's filter and adjustments. The clamps are
//! `Editor::edit_clip_color`'s; the preview grades its frames and export
//! builds FFmpeg filters from the same `openreel_engine::ColorAdjust`.

use eddie_core::Editor;
use openreel_engine::{Clip, ClipId, ColorAdjust, Filter, MAX_EXPOSURE};
use rune::El;

use super::fields::Field;
use super::rows::{button_row, prop_row, section, slider_row};
use super::styles::{Select, select};
use crate::app::Ui;
use crate::theme::{self, FIELD_H, Variant};

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let color = clip.color;
    let level = |label, field, value: f64, set, help| {
        slider_row(
            ui,
            id,
            label,
            field,
            value as f32,
            -1.0..=1.0,
            value_w,
            set,
            help,
        )
    };
    let mut rows = vec![filter(ui, color.filter)];
    if color.filter != Filter::None {
        rows.push(slider_row(
            ui,
            id,
            "Strength",
            Field::Strength,
            color.strength as f32,
            0.0..=1.0,
            value_w,
            |e, id, v| e.edit_clip_color(id, |c| c.strength = f64::from(v)),
            "How much of the filter to apply",
        ));
    }
    rows.extend([
        level(
            "Brightness",
            Field::Brightness,
            color.brightness,
            |e, id, v| e.edit_clip_color(id, |c| c.brightness = f64::from(v)),
            "Lighten or darken the midtones; black and white stay put",
        ),
        level(
            "Contrast",
            Field::Contrast,
            color.contrast,
            |e, id, v| e.edit_clip_color(id, |c| c.contrast = f64::from(v)),
            "Spread lights and darks apart, or draw them together",
        ),
        level(
            "Saturation",
            Field::Saturation,
            color.saturation,
            |e, id, v| e.edit_clip_color(id, |c| c.saturation = f64::from(v)),
            "More or less colour; -100% is black and white",
        ),
        slider_row(
            ui,
            id,
            "Exposure",
            Field::Exposure,
            color.exposure as f32,
            -MAX_EXPOSURE as f32..=MAX_EXPOSURE as f32,
            value_w,
            set_exposure,
            "More or less light, in stops",
        ),
        level(
            "Temperature",
            Field::Temperature,
            color.temperature,
            |e, id, v| e.edit_clip_color(id, |c| c.temperature = f64::from(v)),
            "Warmer (orange) or cooler (blue)",
        ),
        level(
            "Tint",
            Field::Tint,
            color.tint,
            |e, id, v| e.edit_clip_color(id, |c| c.tint = f64::from(v)),
            "Towards magenta, or towards green",
        ),
        reset(id, card_inner),
    ]);
    section("Colour", rows)
}

fn set_exposure(editor: &mut Editor, id: ClipId, stops: f32) {
    editor.edit_clip_color(id, |c| c.exposure = f64::from(stops));
}

fn filter(ui: &Ui, current: Filter) -> El {
    let help = "A preset look, applied before the adjustments below";
    prop_row(
        "Filter",
        help,
        select(
            "details-filter",
            current.label(),
            help,
            Select::Filter,
            ui.details.select == Some(Select::Filter),
        )
        .grow(1.0)
        .min_w(0.0),
    )
    .h(FIELD_H + 1.0)
    .pb(1.0)
}

fn reset(id: ClipId, card_inner: f32) -> El {
    button_row(
        vec![
            theme::button(
                "details-color-reset",
                "Reset",
                Variant::Secondary,
                "Take off the filter and every adjustment",
                true,
            )
            .on_press(move |ui: &mut Ui| {
                ui.editor
                    .edit_clip_color(id, |c| *c = ColorAdjust::default());
            }),
        ],
        card_inner,
    )
    .py(2.0)
}

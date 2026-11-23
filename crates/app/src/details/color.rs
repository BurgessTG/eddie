//! The Colour card: a video clip's filter tiles and adjustments. The clamps are
//! `Editor::edit_clip_color`'s; the preview grades its frames and export
//! builds FFmpeg filters from the same `openreel_engine::ColorAdjust`.

use eddie_core::Editor;
use openreel_engine::{Clip, ClipId, ColorAdjust, Filter, MAX_EXPOSURE, Property};
use rune::El;

use super::fields::Field;
use super::filters;
use super::keys::{self, KEY_SPACE};
use super::rows::{button_row, section, slider_row};
use crate::app::Ui;
use crate::theme::{self, Variant};

/// One adjustment's slider and its key button: `key` names the button
/// `details-key-{key}`, `what` its tooltip.
struct Level {
    label: &'static str,
    field: Field,
    property: Property,
    key: &'static str,
    what: &'static str,
    set: fn(&mut Editor, ClipId, f32),
    help: &'static str,
}

static STRENGTH: Level = Level {
    label: "Strength",
    field: Field::Strength,
    property: Property::FilterStrength,
    key: "details-key-filter-strength",
    what: "filter strength",
    set: |e, id, v| e.edit_clip_color(id, |c| c.strength = f64::from(v)),
    help: "How much of the filter to apply",
};

static LEVELS: [Level; 6] = [
    Level {
        label: "Brightness",
        field: Field::Brightness,
        property: Property::Brightness,
        key: "details-key-brightness",
        what: "brightness",
        set: |e, id, v| e.edit_clip_color(id, |c| c.brightness = f64::from(v)),
        help: "Lighten or darken the midtones; black and white stay put",
    },
    Level {
        label: "Contrast",
        field: Field::Contrast,
        property: Property::Contrast,
        key: "details-key-contrast",
        what: "contrast",
        set: |e, id, v| e.edit_clip_color(id, |c| c.contrast = f64::from(v)),
        help: "Spread lights and darks apart, or draw them together",
    },
    Level {
        label: "Saturation",
        field: Field::Saturation,
        property: Property::Saturation,
        key: "details-key-saturation",
        what: "saturation",
        set: |e, id, v| e.edit_clip_color(id, |c| c.saturation = f64::from(v)),
        help: "More or less colour; -100% is black and white",
    },
    Level {
        label: "Exposure",
        field: Field::Exposure,
        property: Property::Exposure,
        key: "details-key-exposure",
        what: "exposure",
        set: |e, id, v| e.edit_clip_color(id, |c| c.exposure = f64::from(v)),
        help: "More or less light, in stops",
    },
    Level {
        label: "Temperature",
        field: Field::Temperature,
        property: Property::Temperature,
        key: "details-key-temperature",
        what: "temperature",
        set: |e, id, v| e.edit_clip_color(id, |c| c.temperature = f64::from(v)),
        help: "Warmer (orange) or cooler (blue)",
    },
    Level {
        label: "Tint",
        field: Field::Tint,
        property: Property::Tint,
        key: "details-key-tint",
        what: "tint",
        set: |e, id, v| e.edit_clip_color(id, |c| c.tint = f64::from(v)),
        help: "Towards magenta, or towards green",
    },
];

pub(super) fn card(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32, card_inner: f32) -> El {
    let color = clip.color;
    let level = |level: &'static Level| {
        let value = color.value(level.property).unwrap_or(0.0);
        let range = if level.property == Property::Exposure {
            -MAX_EXPOSURE as f32..=MAX_EXPOSURE as f32
        } else if level.property == Property::FilterStrength {
            0.0..=1.0
        } else {
            -1.0..=1.0
        };
        slider_row(
            ui,
            id,
            level.label,
            level.field,
            value as f32,
            range,
            value_w - KEY_SPACE,
            level.set,
            level.help,
        )
        .child(keys::button(
            ui,
            id,
            clip,
            std::slice::from_ref(&level.property),
            level.key,
            level.what,
        ))
    };
    let mut rows = vec![filters::grid(ui, id, color.filter, card_inner)];
    if color.filter != Filter::None {
        rows.push(level(&STRENGTH));
    }
    rows.extend(LEVELS.iter().map(level));
    rows.push(reset(id, card_inner));
    section("Colour", rows)
}

fn reset(id: ClipId, card_inner: f32) -> El {
    button_row(
        vec![
            theme::button(
                "details-color-reset",
                "Reset",
                Variant::Secondary,
                "Take off the filter and every adjustment, with their keys",
                true,
            )
            .on_press(move |ui: &mut Ui| {
                if let Some(clip) = ui.editor.timeline.clip_mut(id) {
                    for property in Property::COLOR {
                        clip.keyframes.clear(property);
                    }
                }
                ui.editor
                    .edit_clip_color(id, |c| *c = ColorAdjust::default());
            }),
        ],
        card_inner,
    )
    .py(2.0)
}

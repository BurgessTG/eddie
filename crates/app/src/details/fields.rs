//! The typed values of the selected clip: which there are, how each reads
//! and applies, and when what was typed reaches the clip.

use eddie_core::Editor;
use openreel_engine::{
    Clip, ClipId, ClipKind, DEFAULT_TRANSITION, Graphic, Rgb, Shape, Transition,
};
use rune::TextEdit;

use super::typed_name;
use crate::app::Ui;

/// A typed value of the selected clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Start,
    Duration,
    SourceIn,
    Speed,
    Opacity,
    Volume,
    FadeIn,
    FadeOut,
    Scale,
    X,
    Y,
    Rotation,
    CropLeft,
    CropTop,
    CropRight,
    CropBottom,
    Brightness,
    Contrast,
    Saturation,
    Exposure,
    Temperature,
    Tint,
    Strength,
    TransitionLength,
    TextSize,
    TextColor,
    BoxColor,
    BoxOpacity,
    OutlineColor,
    Outline,
    ShadowColor,
    ShadowOpacity,
    ShadowDistance,
    ShadowAngle,
    ShadowBlur,
    TextX,
    TextY,
    KeyColor,
    KeyStrength,
    KeySoftness,
    KeySpill,
    MaskX,
    MaskY,
    MaskWidth,
    MaskHeight,
    MaskRotation,
    MaskFeather,
    MaskRound,
    ShapeFill,
    ShapeOutline,
    ShapeThickness,
    ShapeRound,
    ShapeRatio,
}

impl Field {
    pub(super) const ALL: [Self; 53] = [
        Self::Start,
        Self::Duration,
        Self::SourceIn,
        Self::Speed,
        Self::Opacity,
        Self::Volume,
        Self::FadeIn,
        Self::FadeOut,
        Self::Scale,
        Self::X,
        Self::Y,
        Self::Rotation,
        Self::CropLeft,
        Self::CropTop,
        Self::CropRight,
        Self::CropBottom,
        Self::Brightness,
        Self::Contrast,
        Self::Saturation,
        Self::Exposure,
        Self::Temperature,
        Self::Tint,
        Self::Strength,
        Self::TransitionLength,
        Self::TextSize,
        Self::TextColor,
        Self::BoxColor,
        Self::BoxOpacity,
        Self::OutlineColor,
        Self::Outline,
        Self::ShadowColor,
        Self::ShadowOpacity,
        Self::ShadowDistance,
        Self::ShadowAngle,
        Self::ShadowBlur,
        Self::TextX,
        Self::TextY,
        Self::KeyColor,
        Self::KeyStrength,
        Self::KeySoftness,
        Self::KeySpill,
        Self::MaskX,
        Self::MaskY,
        Self::MaskWidth,
        Self::MaskHeight,
        Self::MaskRotation,
        Self::MaskFeather,
        Self::MaskRound,
        Self::ShapeFill,
        Self::ShapeOutline,
        Self::ShapeThickness,
        Self::ShapeRound,
        Self::ShapeRatio,
    ];

    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Start => "details-start",
            Self::Duration => "details-duration",
            Self::SourceIn => "details-source-in",
            Self::Speed => "details-speed",
            Self::Opacity => "details-opacity",
            Self::Volume => "details-volume",
            Self::FadeIn => "details-fade-in",
            Self::FadeOut => "details-fade-out",
            Self::Scale => "details-scale",
            Self::X => "details-x",
            Self::Y => "details-y",
            Self::Rotation => "details-rotation",
            Self::CropLeft => "details-crop-left",
            Self::CropTop => "details-crop-top",
            Self::CropRight => "details-crop-right",
            Self::CropBottom => "details-crop-bottom",
            Self::Brightness => "details-brightness",
            Self::Contrast => "details-contrast",
            Self::Saturation => "details-saturation",
            Self::Exposure => "details-exposure",
            Self::Temperature => "details-temperature",
            Self::Tint => "details-tint",
            Self::Strength => "details-strength",
            Self::TransitionLength => "details-transition-length",
            Self::TextSize => "details-text-size",
            Self::TextColor => "details-text-color",
            Self::BoxColor => "details-box-color",
            Self::BoxOpacity => "details-box-opacity",
            Self::OutlineColor => "details-outline-color",
            Self::Outline => "details-outline",
            Self::ShadowColor => "details-shadow-color",
            Self::ShadowOpacity => "details-shadow-opacity",
            Self::ShadowDistance => "details-shadow-distance",
            Self::ShadowAngle => "details-shadow-angle",
            Self::ShadowBlur => "details-shadow-blur",
            Self::TextX => "details-text-x",
            Self::TextY => "details-text-y",
            Self::KeyColor => "details-key-color",
            Self::KeyStrength => "details-key-strength",
            Self::KeySoftness => "details-key-softness",
            Self::KeySpill => "details-key-spill",
            Self::MaskX => "details-mask-x",
            Self::MaskY => "details-mask-y",
            Self::MaskWidth => "details-mask-width",
            Self::MaskHeight => "details-mask-height",
            Self::MaskRotation => "details-mask-rotation",
            Self::MaskFeather => "details-mask-feather",
            Self::MaskRound => "details-mask-round",
            Self::ShapeFill => "details-shape-fill",
            Self::ShapeOutline => "details-shape-outline",
            Self::ShapeThickness => "details-shape-thickness",
            Self::ShapeRound => "details-shape-round",
            Self::ShapeRatio => "details-shape-ratio",
        }
    }

    /// A colour, shown and typed as `#RRGGBB`.
    fn colour(self) -> bool {
        matches!(
            self,
            Self::TextColor
                | Self::BoxColor
                | Self::OutlineColor
                | Self::ShadowColor
                | Self::KeyColor
                | Self::ShapeFill
                | Self::ShapeOutline
        )
    }

    /// A colour that may be none, shown and typed as "None" (a value
    /// below 0).
    fn optional_colour(self) -> bool {
        matches!(self, Self::ShapeFill | Self::ShapeOutline)
    }

    fn percent(self) -> bool {
        !matches!(
            self,
            Self::Start
                | Self::Duration
                | Self::SourceIn
                | Self::Speed
                | Self::FadeIn
                | Self::FadeOut
                | Self::TransitionLength
                | Self::Rotation
                | Self::MaskRotation
                | Self::ShadowAngle
                | Self::Exposure
                | Self::TextColor
                | Self::BoxColor
                | Self::OutlineColor
                | Self::ShadowColor
                | Self::KeyColor
                | Self::ShapeFill
                | Self::ShapeOutline
                | Self::ShapeRatio
        )
    }

    /// The clip's value in the unit the field shows: seconds, percent,
    /// degrees or stops.
    pub(super) fn value(self, clip: &Clip) -> f64 {
        match self {
            Self::Start => clip.start,
            Self::Duration => clip.duration,
            Self::SourceIn => clip.source_in,
            Self::Speed => clip.speed,
            Self::Opacity => f64::from(clip.opacity) * 100.0,
            Self::Volume => f64::from(clip.volume) * 100.0,
            Self::FadeIn => clip.fade_in,
            Self::FadeOut => clip.fade_out,
            Self::Scale => clip.transform.scale * 100.0,
            Self::X => clip.transform.x * 100.0,
            Self::Y => clip.transform.y * 100.0,
            Self::Rotation => clip.transform.rotation,
            Self::CropLeft => clip.transform.crop_left * 100.0,
            Self::CropTop => clip.transform.crop_top * 100.0,
            Self::CropRight => clip.transform.crop_right * 100.0,
            Self::CropBottom => clip.transform.crop_bottom * 100.0,
            Self::Brightness => clip.color.brightness * 100.0,
            Self::Contrast => clip.color.contrast * 100.0,
            Self::Saturation => clip.color.saturation * 100.0,
            Self::Exposure => clip.color.exposure,
            Self::Temperature => clip.color.temperature * 100.0,
            Self::Tint => clip.color.tint * 100.0,
            Self::Strength => clip.color.strength * 100.0,
            Self::TransitionLength => clip
                .transition
                .map_or(DEFAULT_TRANSITION, |transition| transition.duration),
            Self::TextSize => clip.text.size * 100.0,
            Self::TextX => clip.text.x * 100.0,
            Self::TextY => clip.text.y * 100.0,
            Self::TextColor | Self::BoxColor | Self::OutlineColor | Self::ShadowColor => {
                let drawn = openreel_media::drawn_text(clip);
                let colour = match self {
                    Self::TextColor => drawn.color,
                    Self::BoxColor => drawn.box_color,
                    Self::ShadowColor => drawn.shadow_color,
                    _ => drawn.outline_color,
                };
                let Rgb(r, g, b) = colour.unwrap_or(Rgb::BLACK);
                f64::from(u32::from_be_bytes([0, r, g, b]))
            }
            Self::BoxOpacity => openreel_media::drawn_text(clip).box_opacity.unwrap_or(0.0) * 100.0,
            Self::Outline => openreel_media::drawn_text(clip).outline.unwrap_or(0.0) * 100.0,
            Self::ShadowOpacity => {
                openreel_media::drawn_text(clip)
                    .shadow_opacity
                    .unwrap_or(0.0)
                    * 100.0
            }
            Self::ShadowDistance => clip.text.shadow_distance * 100.0,
            Self::ShadowAngle => clip.text.shadow_angle,
            Self::ShadowBlur => clip.text.shadow_blur * 100.0,
            Self::KeyColor => {
                let Rgb(r, g, b) = clip.chroma.unwrap_or_default().color;
                f64::from(u32::from_be_bytes([0, r, g, b]))
            }
            Self::KeyStrength => clip.chroma.unwrap_or_default().strength * 100.0,
            Self::KeySoftness => clip.chroma.unwrap_or_default().softness * 100.0,
            Self::KeySpill => clip.chroma.unwrap_or_default().spill * 100.0,
            Self::MaskX => clip.mask.unwrap_or_default().x * 100.0,
            Self::MaskY => clip.mask.unwrap_or_default().y * 100.0,
            Self::MaskWidth => clip.mask.unwrap_or_default().width * 100.0,
            Self::MaskHeight => clip.mask.unwrap_or_default().height * 100.0,
            Self::MaskRotation => clip.mask.unwrap_or_default().rotation,
            Self::MaskFeather => clip.mask.unwrap_or_default().feather * 100.0,
            Self::MaskRound => clip.mask.unwrap_or_default().round * 100.0,
            Self::ShapeFill | Self::ShapeOutline => {
                let shape = shape(clip);
                let colour = if self == Self::ShapeFill {
                    shape.fill
                } else {
                    shape.outline
                };
                colour.map_or(-1.0, |Rgb(r, g, b)| {
                    f64::from(u32::from_be_bytes([0, r, g, b]))
                })
            }
            Self::ShapeThickness => shape(clip).thickness * 100.0,
            Self::ShapeRound => shape(clip).round * 100.0,
            Self::ShapeRatio => shape(clip).ratio,
        }
    }

    fn show(self, clip: &Clip) -> String {
        let value = self.value(clip);
        // Whole numbers, and no "-0%" from a hair left of centre.
        let whole = || if value.abs() < 0.5 { 0.0 } else { value };
        if self.colour() && value < 0.0 {
            "None".to_owned()
        } else if self.colour() {
            format!("#{:06X}", value as u32)
        } else if self == Self::ShapeRatio {
            format!("{value:.2}")
        } else if self.percent() {
            format!("{:.0}%", whole())
        } else if matches!(
            self,
            Self::Rotation | Self::MaskRotation | Self::ShadowAngle
        ) {
            format!("{:.0}°", whole())
        } else if self == Self::Exposure {
            let value = if value.abs() < 0.05 { 0.0 } else { value };
            format!("{value:.1} EV")
        } else if self == Self::Speed {
            format!("{value:.2}×")
        } else {
            format!("{value:.2} s")
        }
    }

    /// A typed number, with or without the unit the field shows.
    fn parse(self, typed: &str) -> Option<f64> {
        let typed = typed.trim();
        if self.optional_colour() && typed.eq_ignore_ascii_case("none") {
            return Some(-1.0);
        }
        if self.colour() {
            let Rgb(r, g, b) = Rgb::parse(typed)?;
            return Some(f64::from(u32::from_be_bytes([0, r, g, b])));
        }
        let number = if self == Self::ShapeRatio {
            typed
        } else if self.percent() {
            typed.strip_suffix('%').unwrap_or(typed)
        } else if matches!(
            self,
            Self::Rotation | Self::MaskRotation | Self::ShadowAngle
        ) {
            typed.strip_suffix('°').unwrap_or(typed)
        } else if self == Self::Speed {
            typed.trim_end_matches(['×', 'x', 'X'])
        } else if self == Self::Exposure {
            let unit = typed
                .len()
                .checked_sub(2)
                .filter(|&at| typed.is_char_boundary(at) && typed[at..].eq_ignore_ascii_case("ev"));
            unit.map_or(typed, |at| &typed[..at])
        } else {
            typed.strip_suffix('s').unwrap_or(typed)
        };
        number
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
    }

    fn apply(self, editor: &mut Editor, id: ClipId, value: f64) {
        match self {
            Self::Start => editor.set_clip_start(id, value),
            Self::Duration => editor.set_clip_duration(id, value),
            Self::SourceIn => editor.set_clip_source_in(id, value),
            Self::Speed => editor.set_clip_speed(id, value),
            Self::Opacity => editor.set_clip_opacity(id, (value / 100.0) as f32),
            Self::Volume => editor.set_clip_volume(id, (value / 100.0) as f32),
            Self::FadeIn => editor.set_clip_fades(id, Some(value), None),
            Self::FadeOut => editor.set_clip_fades(id, None, Some(value)),
            Self::Scale => editor.edit_clip_transform(id, |t| t.scale = value / 100.0),
            Self::X => editor.edit_clip_transform(id, |t| t.x = value / 100.0),
            Self::Y => editor.edit_clip_transform(id, |t| t.y = value / 100.0),
            Self::Rotation => editor.edit_clip_transform(id, |t| t.rotation = value),
            Self::CropLeft => editor.edit_clip_transform(id, |t| t.crop_left = value / 100.0),
            Self::CropTop => editor.edit_clip_transform(id, |t| t.crop_top = value / 100.0),
            Self::CropRight => editor.edit_clip_transform(id, |t| t.crop_right = value / 100.0),
            Self::CropBottom => editor.edit_clip_transform(id, |t| t.crop_bottom = value / 100.0),
            Self::Brightness => editor.edit_clip_color(id, |c| c.brightness = value / 100.0),
            Self::Contrast => editor.edit_clip_color(id, |c| c.contrast = value / 100.0),
            Self::Saturation => editor.edit_clip_color(id, |c| c.saturation = value / 100.0),
            Self::Exposure => editor.edit_clip_color(id, |c| c.exposure = value),
            Self::Temperature => editor.edit_clip_color(id, |c| c.temperature = value / 100.0),
            Self::Tint => editor.edit_clip_color(id, |c| c.tint = value / 100.0),
            Self::Strength => editor.edit_clip_color(id, |c| c.strength = value / 100.0),
            Self::TransitionLength => {
                let kept = editor.timeline.clip(id).and_then(|clip| clip.transition);
                editor.set_clip_transition(id, kept.map(|t| Transition::new(t.kind, value)));
            }
            Self::TextSize => editor.edit_title_text(id, |t| t.size = value / 100.0),
            Self::TextX => editor.edit_title_text(id, |t| t.x = value / 100.0),
            Self::TextY => editor.edit_title_text(id, |t| t.y = value / 100.0),
            Self::BoxOpacity => editor.edit_title_text(id, |t| t.box_opacity = Some(value / 100.0)),
            Self::Outline => editor.edit_title_text(id, |t| t.outline = Some(value / 100.0)),
            Self::ShadowOpacity => {
                editor.edit_title_text(id, |t| t.shadow_opacity = Some(value / 100.0));
            }
            Self::ShadowDistance => {
                editor.edit_title_text(id, |t| t.shadow_distance = value / 100.0);
            }
            Self::ShadowAngle => editor.edit_title_text(id, |t| t.shadow_angle = value),
            Self::ShadowBlur => editor.edit_title_text(id, |t| t.shadow_blur = value / 100.0),
            Self::TextColor | Self::BoxColor | Self::OutlineColor | Self::ShadowColor => {
                let [_, r, g, b] = (value as u32).to_be_bytes();
                set_colour(editor, id, self, Rgb(r, g, b));
            }
            Self::KeyColor => {
                let [_, r, g, b] = (value as u32).to_be_bytes();
                editor.edit_clip_chroma(id, |k| k.get_or_insert_default().color = Rgb(r, g, b));
            }
            Self::KeyStrength => editor.edit_clip_chroma(id, |k| {
                k.get_or_insert_default().strength = value / 100.0;
            }),
            Self::KeySoftness => editor.edit_clip_chroma(id, |k| {
                k.get_or_insert_default().softness = value / 100.0;
            }),
            Self::KeySpill => editor.edit_clip_chroma(id, |k| {
                k.get_or_insert_default().spill = value / 100.0;
            }),
            Self::MaskX
            | Self::MaskY
            | Self::MaskWidth
            | Self::MaskHeight
            | Self::MaskRotation
            | Self::MaskFeather
            | Self::MaskRound => editor.edit_clip_mask(id, |mask| {
                let mask = mask.get_or_insert_default();
                let share = value / 100.0;
                match self {
                    Self::MaskX => mask.x = share,
                    Self::MaskY => mask.y = share,
                    Self::MaskWidth => mask.width = share,
                    Self::MaskHeight => mask.height = share,
                    Self::MaskRotation => mask.rotation = value,
                    Self::MaskFeather => mask.feather = share,
                    _ => mask.round = share,
                }
            }),
            Self::ShapeFill | Self::ShapeOutline => {
                let [_, r, g, b] = (value.max(0.0) as u32).to_be_bytes();
                let colour = (value >= 0.0).then_some(Rgb(r, g, b));
                editor.edit_clip_shape(id, |shape| match self {
                    Self::ShapeFill => shape.fill = colour,
                    _ => shape.outline = colour,
                });
            }
            Self::ShapeThickness => editor.edit_clip_shape(id, |s| s.thickness = value / 100.0),
            Self::ShapeRound => editor.edit_clip_shape(id, |s| s.round = value / 100.0),
            Self::ShapeRatio => editor.edit_clip_shape(id, |s| s.ratio = value),
        }
    }
}

/// A shape clip's shape, or a plain rectangle for any other clip (whose
/// Details has no Shape card).
fn shape(clip: &Clip) -> Shape {
    match &clip.graphic {
        Some(Graphic::Shape(shape)) => *shape,
        _ => Shape::default(),
    }
}

/// Give a title's text, box, outline or shadow (`field`) a colour. A box,
/// outline or shadow turned off comes back on to show it.
pub(super) fn set_colour(editor: &mut Editor, id: ClipId, field: Field, colour: Rgb) {
    editor.edit_title_text(id, |t| match field {
        Field::BoxColor => {
            t.box_color = Some(colour);
            if t.box_opacity == Some(0.0) {
                t.box_opacity = None;
            }
        }
        Field::OutlineColor => {
            t.outline_color = Some(colour);
            if t.outline == Some(0.0) {
                t.outline = None;
            }
        }
        Field::ShadowColor => {
            t.shadow_color = Some(colour);
            if t.shadow_opacity == Some(0.0) {
                t.shadow_opacity = None;
            }
        }
        _ => t.color = Some(colour),
    });
}

/// A value field and the text it was last filled with, so only a field the
/// user changed is applied: "2.00 s" read back would round the clip.
pub(super) struct Entry {
    pub(super) edit: TextEdit,
    pub(super) filled: String,
}

/// Once per frame, before the editor's own: apply a value field the
/// keyboard just left, follow the selection, and show the clip's values in
/// every field that is not being typed in.
pub fn sync(ui: &mut Ui) {
    let selected = ui
        .editor
        .selected
        .filter(|id| ui.editor.timeline.clip(*id).is_some());
    if !ui.show_inspector || selected.is_none() {
        // The fields are gone; none of them may keep plain keys.
        ui.details.blur();
    }
    let focused = Field::ALL
        .into_iter()
        .find(|field| ui.details.entry(*field).edit.focused);
    if let Some(left) = ui.details.focused.filter(|left| Some(*left) != focused) {
        commit(ui, left);
    }
    if let Some(entered) = focused.filter(|entered| ui.details.focused != Some(*entered)) {
        // A click selects the value, so typing replaces it.
        let edit = &mut ui.details.entry_mut(entered).edit;
        let end = edit.text.len();
        edit.set_selection(0, end);
    }
    ui.details.focused = focused;

    if ui.key_pick.is_some() && (ui.key_pick != selected || !ui.show_inspector) {
        // Picking is for the clip Details shows.
        ui.key_pick = None;
    }
    let changed = ui.details.shown != selected;
    if changed {
        ui.details.shown = selected;
        ui.details.select = None;
        ui.details.slider_held = false;
    }
    let Some(id) = selected else {
        ui.focus_title = false;
        return;
    };
    fill(ui, id, changed);
    if std::mem::take(&mut ui.focus_title)
        && ui.show_inspector
        && ui
            .editor
            .timeline
            .clip(id)
            .is_some_and(|clip| clip.kind == ClipKind::Title)
    {
        let name = &mut ui.details.name;
        let end = name.text.len();
        name.set_caret(end);
        name.focused = true;
    }
}

/// Show the clip's values. A field being typed in keeps its text unless
/// `all` (another clip is now shown); the name goes to the clip as typed.
fn fill(ui: &mut Ui, id: ClipId, all: bool) {
    let playhead = ui.editor.playhead;
    let Some(clip) = ui.editor.timeline.clip_mut(id) else {
        return;
    };
    let name = &mut ui.details.name;
    if name.focused && !all {
        let typed = typed_name(clip.kind, name.text.as_str());
        if clip.name != typed {
            clip.name = typed;
        }
    } else if name.text.as_str() != clip.name {
        name.replace_text(clip.name.clone());
    }
    // Keyed values as they are under the playhead.
    let clip = clip.posed_at(playhead);
    for field in Field::ALL {
        let entry = &mut ui.details.entries[field as usize];
        if entry.edit.focused && !all {
            continue;
        }
        let shown = field.show(&clip);
        if entry.filled != shown || entry.edit.text.as_str() != shown {
            entry.edit.replace_text(shown.clone());
            entry.filled = shown;
        }
    }
}

/// Apply what was typed in `field`, if anything was, then show what the
/// clip holds now. That also undoes an entry that is not a number.
pub(super) fn commit(ui: &mut Ui, field: Field) {
    let Some(id) = ui.details.shown else {
        return;
    };
    let entry = ui.details.entry(field);
    if entry.edit.text.as_str() != entry.filled {
        let typed = entry.edit.text.trim().to_owned();
        let value = field.parse(&typed).or_else(|| {
            // Start also reads what the playhead's jump box reads.
            (field == Field::Start)
                .then(|| ui.editor.timeline.time_index().time(&typed).ok())
                .flatten()
        });
        if let Some(value) = value {
            field.apply(&mut ui.editor, id, value);
        }
    }
    if let Some(clip) = ui.editor.timeline.clip(id) {
        let shown = field.show(&clip.posed_at(ui.editor.playhead));
        let entry = ui.details.entry_mut(field);
        entry.edit.replace_text(shown.clone());
        entry.filled = shown;
    }
}

/// Esc leaves the clip as it was.
pub(super) fn revert(ui: &mut Ui, field: Field) {
    let entry = ui.details.entry_mut(field);
    let filled = entry.filled.clone();
    entry.edit.replace_text(filled);
}

//! The typed values of the selected clip: which there are, how each reads
//! and applies, and when what was typed reaches the clip.

use eddie_core::Editor;
use openreel_engine::{Clip, ClipId, ClipKind, DEFAULT_TRANSITION, Rgb, Transition};
use rune::TextEdit;

use super::one_line;
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
    TextX,
    TextY,
}

impl Field {
    pub(super) const ALL: [Self; 32] = [
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
        Self::TextX,
        Self::TextY,
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
            Self::TextX => "details-text-x",
            Self::TextY => "details-text-y",
        }
    }

    /// A colour, shown and typed as `#RRGGBB`.
    fn colour(self) -> bool {
        matches!(self, Self::TextColor | Self::BoxColor | Self::OutlineColor)
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
                | Self::Exposure
                | Self::TextColor
                | Self::BoxColor
                | Self::OutlineColor
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
            Self::TextColor | Self::BoxColor | Self::OutlineColor => {
                let drawn = openreel_media::drawn_text(clip);
                let colour = match self {
                    Self::TextColor => drawn.color,
                    Self::BoxColor => drawn.box_color,
                    _ => drawn.outline_color,
                };
                let Rgb(r, g, b) = colour.unwrap_or(Rgb::BLACK);
                f64::from(u32::from_be_bytes([0, r, g, b]))
            }
            Self::BoxOpacity => openreel_media::drawn_text(clip).box_opacity.unwrap_or(0.0) * 100.0,
            Self::Outline => openreel_media::drawn_text(clip).outline.unwrap_or(0.0) * 100.0,
        }
    }

    fn show(self, clip: &Clip) -> String {
        let value = self.value(clip);
        // Whole numbers, and no "-0%" from a hair left of centre.
        let whole = || if value.abs() < 0.5 { 0.0 } else { value };
        if self.colour() {
            format!("#{:06X}", value as u32)
        } else if self.percent() {
            format!("{:.0}%", whole())
        } else if self == Self::Rotation {
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
        if self.colour() {
            let Rgb(r, g, b) = Rgb::parse(typed)?;
            return Some(f64::from(u32::from_be_bytes([0, r, g, b])));
        }
        let number = if self.percent() {
            typed.strip_suffix('%').unwrap_or(typed)
        } else if self == Self::Rotation {
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
            Self::TextColor | Self::BoxColor | Self::OutlineColor => {
                let [_, r, g, b] = (value as u32).to_be_bytes();
                set_colour(editor, id, self, Rgb(r, g, b));
            }
        }
    }
}

/// Give a title's text, box or outline (`field`) a colour. A box or outline
/// turned off comes back on to show it.
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
        let typed = one_line(name.text.as_str());
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

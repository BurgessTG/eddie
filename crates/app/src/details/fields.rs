//! The typed values of the selected clip: which there are, how each reads
//! and applies, and when what was typed reaches the clip.

use eddie_core::Editor;
use openreel_engine::{Clip, ClipId, ClipKind};
use rune::TextEdit;

use super::one_line;
use crate::app::Ui;

/// A typed value of the selected clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Start,
    Duration,
    SourceIn,
    Opacity,
    Volume,
    FadeIn,
    FadeOut,
}

impl Field {
    const ALL: [Self; 7] = [
        Self::Start,
        Self::Duration,
        Self::SourceIn,
        Self::Opacity,
        Self::Volume,
        Self::FadeIn,
        Self::FadeOut,
    ];

    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Start => "details-start",
            Self::Duration => "details-duration",
            Self::SourceIn => "details-source-in",
            Self::Opacity => "details-opacity",
            Self::Volume => "details-volume",
            Self::FadeIn => "details-fade-in",
            Self::FadeOut => "details-fade-out",
        }
    }

    fn percent(self) -> bool {
        matches!(self, Self::Opacity | Self::Volume)
    }

    /// The clip's value in the unit the field shows: seconds or percent.
    pub(super) fn value(self, clip: &Clip) -> f64 {
        match self {
            Self::Start => clip.start,
            Self::Duration => clip.duration,
            Self::SourceIn => clip.source_in,
            Self::Opacity => f64::from(clip.opacity) * 100.0,
            Self::Volume => f64::from(clip.volume) * 100.0,
            Self::FadeIn => clip.fade_in,
            Self::FadeOut => clip.fade_out,
        }
    }

    fn show(self, clip: &Clip) -> String {
        let value = self.value(clip);
        if self.percent() {
            format!("{value:.0}%")
        } else {
            format!("{value:.2} s")
        }
    }

    /// A typed number, with or without the unit the field shows.
    fn parse(self, typed: &str) -> Option<f64> {
        let typed = typed.trim();
        let number = if self.percent() {
            typed.strip_suffix('%').unwrap_or(typed)
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
            Self::Opacity => editor.set_clip_opacity(id, (value / 100.0) as f32),
            Self::Volume => editor.set_clip_volume(id, (value / 100.0) as f32),
            Self::FadeIn => editor.set_clip_fades(id, Some(value), None),
            Self::FadeOut => editor.set_clip_fades(id, None, Some(value)),
        }
    }
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
    for field in Field::ALL {
        let entry = &mut ui.details.entries[field as usize];
        if entry.edit.focused && !all {
            continue;
        }
        let shown = field.show(clip);
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
        let shown = field.show(clip);
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

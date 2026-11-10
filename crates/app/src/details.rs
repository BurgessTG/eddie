//! The Details panel: the selected clip's name, timing, look and sound.
//! The edits themselves are `eddie_core::editor::details`; this file draws
//! the fields and turns typing, drags and presses into those calls.

use eddie_core::editor::details::MAX_VOLUME;
use eddie_core::{Action, Editor, shortcuts};
use openreel_engine::{Clip, ClipId, ClipKind, TitleStyle};
use rune::{Align, Cursor, Cx, El, Key, Phase, ScrollSpec, Side, TextEdit, col, el, row, text};
use rune_kit::{ScrollBar, ScrollBarStyle};

use crate::app::Ui;
use crate::theme::{self, FIELD_H, Variant, c};

pub const DEFAULT_W: f32 = 340.0;
const MIN_W: f32 = 280.0;
const MAX_W: f32 = 460.0;
/// The panel's side padding; the left border is drawn inside it.
const PAD_X: f32 = 14.0;
/// A card's padding, its border included.
const CARD_PAD: f32 = 13.0;
/// Under a card's last row.
const CARD_PAD_BOTTOM: f32 = 12.0;
/// A card title's line, in whole pixels so the rows under it stay sharp.
const CARD_TITLE_H: f32 = 17.0;
const GAP: f32 = 8.0;
const LABEL_W: f32 = 78.0;
const ROW_H: f32 = 30.0;
const HANDLE: f32 = 24.0;
/// The rail starts this far inside the slider's left end,
const RAIL_INSET: f32 = 4.0;
/// and stops this far short of its right end.
const RAIL_END: f32 = 2.0;
/// The handle's centre stays this far inside the rail's ends.
const HANDLE_REACH: f32 = 11.2;
const PERCENT_W: f32 = 58.0;
const BUTTON_GAP: f32 = 6.0;
/// A text button's height in whole pixels, so its edges stay sharp.
const BUTTON_H: f32 = 27.0;

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

    fn key(self) -> &'static str {
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
    fn value(self, clip: &Clip) -> f64 {
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
struct Entry {
    edit: TextEdit,
    filled: String,
}

/// The style list that is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    /// The selected title's own style.
    Title,
    /// The style generated captions get.
    Captions,
}

pub struct State {
    /// The clip the fields were last filled from.
    pub shown: Option<ClipId>,
    pub name: TextEdit,
    entries: [Entry; 7],
    /// The value field that had the keyboard at the last poll.
    focused: Option<Field>,
    pub select: Option<Select>,
    /// A slider is held, so undo waits for the release.
    pub slider_held: bool,
    pub width: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            shown: None,
            name: TextEdit::new(""),
            entries: std::array::from_fn(|_| Entry {
                edit: TextEdit::new(""),
                filled: String::new(),
            }),
            focused: None,
            select: None,
            slider_held: false,
            width: DEFAULT_W,
        }
    }
}

impl State {
    /// A field here has the keyboard, so plain keys are text.
    pub fn typing(&self) -> bool {
        self.name.focused || self.entries.iter().any(|entry| entry.edit.focused)
    }

    fn entry(&self, field: Field) -> &Entry {
        &self.entries[field as usize]
    }

    fn entry_mut(&mut self, field: Field) -> &mut Entry {
        &mut self.entries[field as usize]
    }

    fn blur(&mut self) {
        self.name.focused = false;
        for entry in &mut self.entries {
            entry.edit.focused = false;
        }
    }
}

/// Export draws a title on one line.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
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
fn commit(ui: &mut Ui, field: Field) {
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
fn revert(ui: &mut Ui, field: Field) {
    let entry = ui.details.entry_mut(field);
    let filled = entry.filled.clone();
    entry.edit.replace_text(filled);
}

// -- view ----------------------------------------------------------------------

pub fn panel(ui: &Ui) -> El {
    let c = c();
    let width = ui.details.width;
    let mut content = col()
        .w_full()
        .h_full()
        .px(PAD_X)
        .py(12.0)
        .child(theme::panel_title("Details"))
        .child(el().h(18.0));
    let clip = ui
        .details
        .shown
        .and_then(|id| Some((id, ui.editor.timeline.clip(id)?)));
    content = match clip {
        Some((id, clip)) => content.child(
            col()
                .key("details-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(body(ui, id, clip))
                .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside()),
        ),
        None => content
            .child(el().h(24.0))
            .child(
                row().key("details-empty").w_full().center().child(
                    text("No clip selected".to_owned())
                        .font(theme::BODY)
                        .color(c.muted_fg)
                        .single_line(),
                ),
            )
            .child(el().h(5.0))
            .child(
                row()
                    .w_full()
                    .center()
                    .child(theme::caption("Click a clip on the timeline to edit it.")),
            ),
    };
    el().key("details")
        .w(width)
        .h_full()
        .fill(c.surface)
        .child(content)
        .child(el().absolute(0.0, 0.0).w(1.0).h_full().fill(c.border))
        .child(
            el().key("details-resize")
                .absolute(-3.0, 0.0)
                .w(7.0)
                .h_full()
                .cursor(Cursor::ResizeColumn)
                .help("Drag to resize Details")
                .on_drag(|ui: &mut Ui, drag, _| {
                    if drag.phase == Phase::Down {
                        drag.data = ui.details.width;
                    } else {
                        ui.details.width = (drag.data - drag.dx).round().clamp(MIN_W, MAX_W);
                    }
                }),
        )
}

fn body(ui: &Ui, id: ClipId, clip: &Clip) -> El {
    let Some(info) = ui.editor.clip_details(id) else {
        return col();
    };
    let inner = ui.details.width - 2.0 * PAD_X;
    let card_inner = inner - 2.0 * CARD_PAD;
    let value_w = card_inner - LABEL_W - GAP;
    let is_audio = clip.kind == ClipKind::Audio;

    let mut timing = vec![
        seconds_row(
            ui,
            "Start",
            Field::Start,
            clip,
            "Where the clip starts on the timeline. Also takes 1:05, 120f or clip:7@end",
        ),
        seconds_row(
            ui,
            "Duration",
            Field::Duration,
            clip,
            "How long the clip plays",
        ),
    ];
    if info.has_source {
        timing.push(seconds_row(
            ui,
            "Source in",
            Field::SourceIn,
            clip,
            "Where playback starts inside the source file",
        ));
    }

    let mut look = Vec::new();
    if !is_audio {
        let row = slider_row(
            ui,
            id,
            "Opacity",
            Field::Opacity,
            (clip.opacity, 1.0),
            value_w,
            Editor::set_clip_opacity,
            "Fades the picture against black",
        );
        look.push(if info.has_source {
            row
        } else {
            last_slider(row)
        });
    }
    if info.has_source {
        let row = slider_row(
            ui,
            id,
            "Volume",
            Field::Volume,
            (clip.volume, MAX_VOLUME),
            value_w,
            Editor::set_clip_volume,
            "Clip gain; 100% is unity",
        );
        look.push(if is_audio { row } else { last_slider(row) });
    }
    if is_audio && info.has_source {
        look.push(prop_row(
            "Fades",
            "Seconds to rise from silence and to fall back to it",
            row()
                .grow(1.0)
                .min_w(0.0)
                .gap(BUTTON_GAP)
                .child(
                    value_field(
                        ui,
                        Field::FadeIn,
                        seconds_help("Fade in: seconds to rise from silence", clip.fade_in),
                    )
                    .w((value_w - BUTTON_GAP) / 2.0),
                )
                .child(
                    value_field(
                        ui,
                        Field::FadeOut,
                        seconds_help("Fade out: seconds to fall to silence", clip.fade_out),
                    )
                    .w((value_w - BUTTON_GAP) / 2.0),
                ),
        ));
        look.push(checkbox(
            id,
            "details-smooth-fades",
            "Smooth fades",
            "Fade on an equal-power curve, which stays fuller through a crossfade",
            clip.smooth_fades,
            |clip| clip.smooth_fades = !clip.smooth_fades,
        ));
        look.push(checkbox(
            id,
            "details-duck",
            "Duck under speech",
            "Dip this sound while another clip is talking",
            clip.duck,
            |clip| clip.duck = !clip.duck,
        ));
        look.push(checkbox(
            id,
            "details-clean-voice",
            "Clean voice",
            "Cut low rumble and even out loud and quiet speech, in preview and export",
            clip.clean_voice,
            |clip| clip.clean_voice = !clip.clean_voice,
        ));
    }
    if clip.kind == ClipKind::Title {
        let help = "Choose how this title or caption is drawn";
        look.push(
            prop_row(
                "Style",
                help,
                select(
                    "details-style",
                    clip.title_style.label(),
                    help,
                    Select::Title,
                    ui.details.select == Some(Select::Title),
                )
                .grow(1.0)
                .min_w(0.0),
            )
            .h(FIELD_H + 1.0)
            .pb(1.0),
        );
    }
    if info.embedded_audio_choice {
        look.push(checkbox(
            id,
            "details-embedded-audio",
            "Embedded audio",
            "Play this video clip's own sound",
            clip.embedded_audio,
            |clip| clip.embedded_audio = !clip.embedded_audio,
        ));
    }
    if let Some(partner) = &info.linked_name {
        look.push(
            theme::caption(format!("Linked to {partner}"))
                .key("details-linked")
                .single_line()
                .truncate()
                .help("Picture and sound move, trim and split together"),
        );
    }
    let mut buttons = Vec::new();
    if info.can_detach {
        buttons.push(
            theme::button(
                "details-detach",
                "Detach audio",
                Variant::Secondary,
                "Move this clip's sound to a linked audio clip you can edit separately",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.editor.detach_clip_audio(id)),
        );
    }
    if info.linked_name.is_some() {
        buttons.push(
            theme::button(
                "details-unlink",
                "Unlink",
                Variant::Secondary,
                "Let picture and sound move independently for J/L cuts or replacement audio",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.editor.unlink_clip(id)),
        );
    }
    if info.can_remove_silence {
        buttons.push(
            theme::button(
                "details-remove-silence",
                "Remove silence",
                Variant::Secondary,
                "Cut waveform pauses from this speech clip and ripple later clips left",
                true,
            )
            .on_press(move |ui: &mut Ui| ui.editor.remove_clip_silence(id)),
        );
    }
    if !buttons.is_empty() {
        look.push(button_row(buttons, card_inner).py(2.0));
    }
    if info.can_caption {
        let help = "Choose the style for generated captions";
        look.push(prop_row(
            "Captions",
            help,
            row()
                .grow(1.0)
                .min_w(0.0)
                .gap(BUTTON_GAP)
                .center_y()
                .child(
                    select(
                        "details-caption-style",
                        ui.editor.caption_style.label(),
                        help,
                        Select::Captions,
                        ui.details.select == Some(Select::Captions),
                    )
                    .grow(1.0)
                    .min_w(0.0),
                )
                .child(
                    theme::button(
                        "details-add-captions",
                        "Add",
                        Variant::Secondary,
                        "Generate caption clips from this clip's transcript in the chosen style",
                        true,
                    )
                    .on_press(move |ui: &mut Ui| ui.editor.add_clip_captions(id)),
                ),
        ));
    }

    let mut body = col()
        .w_full()
        .child(header(ui, clip))
        .child(el().h(16.0))
        .child(section("Timing", timing));
    if !look.is_empty() {
        let title = match clip.kind {
            ClipKind::Video => "Picture & sound",
            ClipKind::Audio => "Sound",
            ClipKind::Title => "Look",
        };
        body = body.child(el().h(14.0)).child(section(title, look));
    }
    body.child(el().h(20.0)).child(button_row(
        vec![
            theme::button(
                "details-split",
                "Split at playhead",
                Variant::Secondary,
                &shortcuts::tip("Split this clip at the playhead", Action::SplitSelected),
                true,
            )
            .on_press(move |ui: &mut Ui| ui.editor.split_clip_at_playhead(id)),
            theme::button(
                "details-delete",
                "Delete",
                Variant::Destructive,
                &shortcuts::tip("Delete this clip", Action::DeleteSelected),
                true,
            )
            .on_press(|ui: &mut Ui| ui.act(Action::DeleteSelected)),
        ],
        inner,
    ))
}

/// The clip's kind as a coloured dot, and its name. A title's name is its
/// on-screen text, so it gets a box that wraps.
fn header(ui: &Ui, clip: &Clip) -> El {
    let c = c();
    // The lanes' clip colours, 1.6 times as bright.
    let (kind, dot) = match clip.kind {
        ClipKind::Video => ("Video", theme::hex("#4A76AB")),
        ClipKind::Audio => ("Audio", theme::hex("#468660")),
        ClipKind::Title => ("Title", theme::hex("#AB8526")),
    };
    let field = rune::text_input(&ui.details.name, theme::BODY)
        .selection_color(theme::selection())
        .key("details-name")
        .color(c.foreground)
        .on_edit(|ui: &mut Ui| &mut ui.details.name);
    let name = if clip.kind == ClipKind::Title {
        let field = field
            .w_full()
            .label("On-screen text")
            // Enter ends the edit; a title has no second line to start.
            .on_input_key(|_: &mut Ui, event, tx| {
                if matches!(event.key, Key::Enter) {
                    tx.blur();
                    true
                } else {
                    false
                }
            })
            .on_input_paste(|ui: &mut Ui, pasted, _| {
                ui.details.name.insert(&one_line(pasted.text()));
                true
            });
        el().key("details-title-box")
            .grow(1.0)
            .min_w(0.0)
            .px(8.0)
            .pt(6.0)
            .pb(4.0)
            .radius(theme::R_MD)
            .fill(c.input)
            .focus_within_ring(1.0, -1.0, c.brand)
            .help("On-screen text")
            .child(rune::auto_grow(field, 2, 6))
    } else {
        let mut help = format!("{kind} clip name");
        if let Some(source) = &clip.source {
            help.push_str(&format!("\nSource: {source}"));
        }
        field
            .single_line()
            .grow(1.0)
            .min_w(0.0)
            .h(FIELD_H)
            .px(8.0)
            .py(6.0)
            .radius(theme::R_MD)
            .fill(c.input)
            .focus_within_ring(1.0, -1.0, c.brand)
            .label(format!("{kind} clip name"))
            .help(help)
            .on_submit(|_: &mut Ui, tx| tx.blur())
    };
    // The dot stays beside the first line when a title's box grows.
    row()
        .w_full()
        .gap(8.0)
        .items_start()
        .child(
            el().size(10.0, FIELD_H).child(
                el().absolute(1.0, FIELD_H / 2.0 - 4.0)
                    .size(8.0, 8.0)
                    .radius(4.0)
                    .fill(dot),
            ),
        )
        .child(name)
}

fn section(title: &str, rows: Vec<El>) -> El {
    theme::card()
        .padding(CARD_PAD)
        .pb(CARD_PAD_BOTTOM)
        .w_full()
        .gap(GAP)
        .child(
            text(title.to_owned())
                .font(theme::SEMIBOLD)
                .color(c().foreground)
                .single_line()
                .h(CARD_TITLE_H),
        )
        .children(rows)
}

/// A label and its control on one line.
fn prop_row(label: &str, help: &str, control: El) -> El {
    row()
        .w_full()
        .h(ROW_H)
        .gap(GAP)
        .center_y()
        .help(help.to_owned())
        .child(
            text(label.to_owned())
                .font(theme::BODY)
                .color(c().muted_fg)
                .single_line()
                .w(LABEL_W),
        )
        .child(control)
}

/// The tooltip of a seconds field: what it is, and the value in full.
fn seconds_help(what: &str, seconds: f64) -> String {
    format!("{what}\n{seconds:.3} s")
}

fn seconds_row(ui: &Ui, label: &str, field: Field, clip: &Clip, what: &str) -> El {
    let help = seconds_help(what, field.value(clip));
    prop_row(
        label,
        &help,
        value_field(ui, field, help.clone()).grow(1.0).min_w(0.0),
    )
}

/// A typed value. Enter or leaving the field applies it; Esc does not.
fn value_field(ui: &Ui, field: Field, help: String) -> El {
    let c = c();
    rune::text_input(&ui.details.entry(field).edit, theme::BODY)
        .selection_color(theme::selection())
        .key(field.key())
        .single_line()
        .align(Align::Center)
        .h(FIELD_H)
        .px(8.0)
        .py(6.0)
        .radius(theme::R_MD)
        .fill(theme::field_fill())
        .color(c.foreground)
        .focus_within_ring(1.0, -1.0, c.brand)
        .help(help)
        .on_edit(rune::edit_access(move |ui: &mut Ui| {
            &mut ui.details.entry_mut(field).edit
        }))
        .on_submit(move |ui: &mut Ui, tx| {
            commit(ui, field);
            tx.blur();
        })
        .on_input_key(move |ui: &mut Ui, event, tx| {
            if matches!(event.key, Key::Escape) {
                revert(ui, field);
                tx.blur();
                true
            } else {
                false
            }
        })
}

/// A 0-to-`max` slider with the same value as a typed percentage beside it.
#[allow(clippy::too_many_arguments)]
fn slider_row(
    ui: &Ui,
    id: ClipId,
    label: &'static str,
    field: Field,
    (value, max): (f32, f32),
    value_w: f32,
    set: fn(&mut Editor, ClipId, f32),
    help: &'static str,
) -> El {
    let c = c();
    let width = value_w - BUTTON_GAP - PERCENT_W;
    let fraction = (value / max).clamp(0.0, 1.0);
    let (low, high) = handle_range(width);
    let x = low + fraction * (high - low);
    let rail = (HANDLE - 8.0) / 2.0;
    let slider = el()
        .key(format!("{}-slider", field.key()))
        .size(width, HANDLE)
        .mb(2.0)
        .child(
            el().absolute(RAIL_INSET, rail)
                .size(width - RAIL_INSET - RAIL_END, 8.0)
                .radius(4.0)
                .fill(c.secondary),
        )
        .child(
            // BRAND at 27% over the rail.
            el().absolute(RAIL_INSET, rail)
                .size((x - RAIL_INSET).max(0.0), 8.0)
                .radius(4.0)
                .fill(theme::hex("#2C4062")),
        )
        .child(
            el().absolute(x - HANDLE / 2.0, 0.0)
                .size(HANDLE, HANDLE)
                .radius(HANDLE / 2.0)
                .fill(c.secondary)
                .border(1.0, theme::hex("#E2E2E2")),
        )
        .instant_changes()
        .cursor(Cursor::Pointer)
        .help(help)
        .label(label)
        .range_value(f64::from(value), 0.0, f64::from(max))
        .on_step(move |ui: &mut Ui, steps: i32, _| {
            set(&mut ui.editor, id, value + steps as f32 * 0.01);
        })
        .on_drag(move |ui: &mut Ui, drag, _| {
            ui.details.slider_held = drag.phase != Phase::Up;
            let (low, high) = handle_range(drag.w);
            if drag.phase == Phase::Up || high <= low {
                return;
            }
            let fraction = ((drag.x - low) / (high - low)).clamp(0.0, 1.0);
            // Whole percents, as the field beside it shows.
            set(&mut ui.editor, id, (fraction * max * 100.0).round() / 100.0);
        });
    prop_row(
        label,
        help,
        row()
            .grow(1.0)
            .min_w(0.0)
            .gap(BUTTON_GAP)
            .center_y()
            .child(slider)
            .child(value_field(ui, field, help.to_owned()).w(PERCENT_W)),
    )
}

/// The last slider row before a shorter row sits one pixel closer to it.
fn last_slider(row: El) -> El {
    row.h(ROW_H - 1.0).pt(1.0)
}

/// Where the handle's centre runs along a slider `width` wide.
fn handle_range(width: f32) -> (f32, f32) {
    (RAIL_INSET + HANDLE_REACH, width - RAIL_END - HANDLE_REACH)
}

/// A switch on the clip itself.
fn checkbox(
    id: ClipId,
    key: &'static str,
    label: &'static str,
    help: &'static str,
    on: bool,
    toggle: fn(&mut Clip),
) -> El {
    let control = theme::checkbox(key, label, help, on).on_press(move |ui: &mut Ui| {
        if let Some(clip) = ui.editor.timeline.clip_mut(id) {
            toggle(clip);
        }
    });
    // The row keeps the press to the box and its label.
    row().w_full().child(control)
}

/// The closed style list: its choice and a down arrow.
fn select(key: &'static str, choice: &str, help: &str, which: Select, open: bool) -> El {
    theme::select(key, choice, help, theme::field_fill(), open, true).on_press(
        move |ui: &mut Ui| {
            ui.details.select = if ui.details.select == Some(which) {
                None
            } else {
                Some(which)
            };
        },
    )
}

/// Buttons sharing `width` equally.
fn button_row(buttons: Vec<El>, width: f32) -> El {
    let count = buttons.len().max(1) as f32;
    let each = ((width - BUTTON_GAP * (count - 1.0)) / count).floor();
    row()
        .w_full()
        .gap(BUTTON_GAP)
        .children(buttons.into_iter().map(|button| button.w(each).h(BUTTON_H)))
}

fn option_key(style: TitleStyle) -> String {
    format!(
        "details-option-{}",
        style.label().to_lowercase().replace(' ', "-")
    )
}

/// One style in the open list.
fn option(which: Select, id: ClipId, style: TitleStyle, current: bool) -> El {
    theme::option(
        option_key(style),
        style.label(),
        &format!("Use the {} style", style.label()),
        current,
    )
    .on_press(move |ui: &mut Ui| {
        match which {
            Select::Title => {
                if let Some(clip) = ui.editor.timeline.clip_mut(id) {
                    clip.title_style = style;
                }
            }
            Select::Captions => ui.editor.caption_style = style,
        }
        ui.details.select = None;
    })
}

/// The open style list, hung under its field. A press outside closes it.
pub fn overlays(ui: &Ui, cx: &Cx<Ui>, overlays: &mut Vec<El>) {
    let Some(which) = ui.details.select else {
        return;
    };
    let Some((id, clip)) = ui
        .details
        .shown
        .and_then(|id| Some((id, ui.editor.timeline.clip(id)?)))
    else {
        return;
    };
    let (anchor, current) = match which {
        Select::Title => ("details-style", clip.title_style),
        Select::Captions => ("details-caption-style", ui.editor.caption_style),
    };
    let close = |ui: &mut Ui| ui.details.select = None;
    overlays.push(rune_kit::scrim("details-select-scrim", close));
    // As wide as its field, wider only when a style's name needs it.
    let width = cx.layout_size(anchor).map_or(0.0, |size| size[0]);
    overlays.push(
        theme::popup("details-select")
            .min_w(width)
            .gap(6.0)
            .children(
                TitleStyle::ALL
                    .into_iter()
                    .map(|style| option(which, id, style, style == current)),
            )
            .anchored(anchor, Side::Bottom, Align::Start, 0.0)
            .modal(true, close),
    );
}

#[cfg(test)]
mod tests {
    use eddie_core::editor::media::{AssetState, BinAsset};
    use eddie_core::editor::sweep::SweepData;
    use rune::{Env, KeyEvent, Mods, Runtime};

    use super::*;
    use crate::app;

    const FRAME: f64 = 1.0 / 60.0;

    /// A window whose only clip is selected: a title from 2 s to 5 s, or
    /// 4 s of a 10 s file with sound, placed from 1 s of the source.
    fn window(kind: ClipKind) -> (Runtime<Ui>, ClipId) {
        let mut ui = Ui::headless();
        ui.editor.assets.push(BinAsset {
            info: openreel_media::MediaInfo {
                path: "/media/a.mp4".to_owned(),
                name: "a.mp4".to_owned(),
                duration: 10.0,
                width: 320,
                height: 180,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: SweepData::default(),
        });
        let id = match kind {
            ClipKind::Title => ui.editor.timeline.add_clip("Title", kind, 0, 2.0, 3.0, 0.0),
            _ => ui
                .editor
                .timeline
                .add_media_clip("Clip", kind, 0, 2.0, 4.0, 1.0, "/media/a.mp4"),
        };
        ui.editor.selected = Some(id);
        let mut rt = Runtime::new(ui, app::view, (1400.0, 1200.0), Env::new(true, 1.0));
        rt.poll = Some(app::poll);
        for _ in 0..3 {
            rt.frame(FRAME);
        }
        (rt, id)
    }

    fn clip(rt: &Runtime<Ui>, id: ClipId) -> &Clip {
        rt.state
            .editor
            .timeline
            .clip(id)
            .expect("the clip is on the timeline")
    }

    fn click(rt: &mut Runtime<Ui>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key} is not shown"));
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_up(x, y);
        rt.frame(FRAME);
        rt.frame(FRAME);
    }

    fn press(rt: &mut Runtime<Ui>, key: Key) {
        rt.key(KeyEvent {
            key,
            mods: Mods::default(),
        });
        rt.frame(FRAME);
        rt.frame(FRAME);
    }

    fn typed(rt: &Runtime<Ui>, field: Field) -> &str {
        rt.state.details.entry(field).edit.text.as_str()
    }

    #[test]
    fn enter_applies_a_typed_value_and_shows_what_the_clip_took() {
        let (mut rt, id) = window(ClipKind::Video);
        assert_eq!(typed(&rt, Field::Duration), "4.00 s");
        click(&mut rt, "details-duration");
        assert!(rt.state.typing());
        rt.paste("3");
        rt.frame(FRAME);
        press(&mut rt, Key::Enter);
        assert_eq!(clip(&rt, id).duration, 3.0);
        assert_eq!(typed(&rt, Field::Duration), "3.00 s");
        assert!(!rt.state.typing());

        // The source is 10 s and the clip starts 1 s into it.
        click(&mut rt, "details-duration");
        rt.paste("60 s");
        rt.frame(FRAME);
        press(&mut rt, Key::Enter);
        assert_eq!(clip(&rt, id).duration, 9.0);
        assert_eq!(typed(&rt, Field::Duration), "9.00 s");
    }

    #[test]
    fn escape_leaves_the_clip_alone_and_leaving_the_field_applies() {
        let (mut rt, id) = window(ClipKind::Video);
        click(&mut rt, "details-start");
        rt.paste("7");
        rt.frame(FRAME);
        press(&mut rt, Key::Escape);
        assert_eq!(clip(&rt, id).start, 2.0);
        assert_eq!(typed(&rt, Field::Start), "2.00 s");
        assert!(!rt.state.typing());

        click(&mut rt, "details-start");
        rt.paste("0:05");
        rt.frame(FRAME);
        click(&mut rt, "details-duration");
        assert_eq!(clip(&rt, id).start, 5.0);
        assert_eq!(typed(&rt, Field::Start), "5.00 s");

        // Text that is no time changes nothing.
        click(&mut rt, "details-start");
        rt.paste("soon");
        rt.frame(FRAME);
        press(&mut rt, Key::Enter);
        assert_eq!(clip(&rt, id).start, 5.0);
        assert_eq!(typed(&rt, Field::Start), "5.00 s");
    }

    #[test]
    fn an_untouched_field_does_not_round_the_clip() {
        let (mut rt, id) = window(ClipKind::Video);
        rt.state.editor.timeline.clip_mut(id).unwrap().source_in = 1.2345;
        rt.frame(FRAME);
        click(&mut rt, "details-source-in");
        click(&mut rt, "details-duration");
        assert_eq!(clip(&rt, id).source_in, 1.2345);
    }

    #[test]
    fn dragging_a_slider_sets_the_value_and_its_percentage() {
        let (mut rt, id) = window(ClipKind::Video);
        let (x, y) = rt.find("details-opacity-slider").expect("the slider");
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_move(x - 20.0, y);
        rt.frame(FRAME);
        assert!(rt.state.pointer_held, "undo waits while the slider is held");
        rt.pointer_move(x - 400.0, y);
        rt.frame(FRAME);
        rt.pointer_up(x - 400.0, y);
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert_eq!(clip(&rt, id).opacity, 0.0);
        assert_eq!(typed(&rt, Field::Opacity), "0%");
        assert!(!rt.state.pointer_held);

        // The field takes more than the slider's top and keeps the limit.
        click(&mut rt, "details-volume");
        rt.paste("350");
        rt.frame(FRAME);
        press(&mut rt, Key::Enter);
        assert_eq!(clip(&rt, id).volume, MAX_VOLUME);
        assert_eq!(typed(&rt, Field::Volume), "200%");
    }

    #[test]
    fn detach_links_a_sound_clip_and_unlink_frees_it() {
        let (mut rt, id) = window(ClipKind::Video);
        assert!(rt.find("details-unlink").is_none());
        click(&mut rt, "details-detach");
        let partner = clip(&rt, id).linked_clip.expect("the sound is linked");
        assert!(!clip(&rt, id).embedded_audio);
        assert!(rt.find("details-detach").is_none());
        assert!(rt.find("details-linked").is_some());
        click(&mut rt, "details-unlink");
        assert_eq!(clip(&rt, id).linked_clip, None);
        assert_eq!(clip(&rt, partner).linked_clip, None);
    }

    #[test]
    fn sound_switches_and_fades_edit_the_clip() {
        let (mut rt, id) = window(ClipKind::Audio);
        assert!(rt.find("details-opacity-slider").is_none());
        let before = clip(&rt, id).duck;
        click(&mut rt, "details-duck");
        assert_eq!(clip(&rt, id).duck, !before);
        click(&mut rt, "details-smooth-fades");
        assert!(clip(&rt, id).smooth_fades);
        click(&mut rt, "details-fade-in");
        rt.paste("0.5");
        rt.frame(FRAME);
        press(&mut rt, Key::Enter);
        assert_eq!(clip(&rt, id).fade_in, 0.5);
        assert_eq!(clip(&rt, id).fade_out, 0.0);
    }

    #[test]
    fn a_title_takes_its_text_as_typed_on_one_line_and_its_style_from_the_list() {
        let (mut rt, id) = window(ClipKind::Title);
        rt.state.focus_title = true;
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert!(rt.state.details.name.focused);
        rt.paste(" card\ntwo");
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert_eq!(clip(&rt, id).name, "Title card two");
        press(&mut rt, Key::Enter);
        assert!(!rt.state.typing());
        assert_eq!(clip(&rt, id).name, "Title card two");

        let style = TitleStyle::ALL[2];
        assert_ne!(clip(&rt, id).title_style, style);
        click(&mut rt, "details-style");
        assert_eq!(rt.state.details.select, Some(Select::Title));
        click(&mut rt, &option_key(style));
        assert_eq!(clip(&rt, id).title_style, style);
        assert_eq!(rt.state.details.select, None);
    }

    #[test]
    fn split_and_delete_act_on_the_selected_clip() {
        let (mut rt, id) = window(ClipKind::Title);
        rt.state.editor.playhead = 3.0;
        rt.frame(FRAME);
        click(&mut rt, "details-split");
        let right = rt
            .state
            .editor
            .selected
            .expect("the right half is selected");
        assert_ne!(right, id);
        assert_eq!(clip(&rt, id).duration, 1.0);
        click(&mut rt, "details-delete");
        assert!(rt.state.editor.timeline.clip(right).is_none());
        assert!(rt.find("details-empty").is_some());
    }

    #[test]
    fn every_control_has_a_tooltip() {
        let cases: [(ClipKind, &[&str]); 3] = [
            (
                ClipKind::Video,
                &[
                    "details-name",
                    "details-start",
                    "details-duration",
                    "details-source-in",
                    "details-opacity-slider",
                    "details-opacity",
                    "details-volume-slider",
                    "details-volume",
                    "details-embedded-audio",
                    "details-detach",
                    "details-remove-silence",
                    "details-split",
                    "details-delete",
                    "details-resize",
                ],
            ),
            (
                ClipKind::Audio,
                &[
                    "details-fade-in",
                    "details-fade-out",
                    "details-smooth-fades",
                    "details-duck",
                    "details-clean-voice",
                ],
            ),
            (ClipKind::Title, &["details-title-box", "details-style"]),
        ];
        for (kind, keys) in cases {
            let (mut rt, _) = window(kind);
            rt.help_delay = 0.0;
            for key in keys {
                let (x, y) = rt
                    .find(key)
                    .unwrap_or_else(|| panic!("{key} is not shown for {kind:?}"));
                // Rest the pointer on the control, coming from where nothing explains itself.
                rt.pointer_move(700.0, 300.0);
                rt.frame(FRAME);
                rt.pointer_move(x, y);
                rt.frame(FRAME);
                rt.frame(FRAME);
                let shown = rt.help_tooltip().map(|(words, _)| words.to_owned());
                assert!(
                    shown.as_deref().is_some_and(|words| !words.is_empty()),
                    "{key} has no tooltip"
                );
            }
        }
    }
}

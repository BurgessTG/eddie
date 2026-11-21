//! The cards Details shows for one clip: its name, Timing, the look and
//! sound controls its kind has, then Split and Delete.

use eddie_core::editor::details::{ClipDetails, MAX_VOLUME};
use eddie_core::{Action, Editor, shortcuts};
use openreel_engine::{Clip, ClipId, ClipKind, MAX_SPEED, MIN_SPEED, Property};
use rune::{El, Key, col, el, row};

use super::fields::Field;
use super::rows::{
    button_row, checkbox, last_slider, prop_row, seconds_help, seconds_row, section, slider_row,
    value_field,
};
use super::styles::{Select, select};
use super::{BUTTON_GAP, CARD_PAD, GAP, LABEL_W, PAD_X, one_line};
use super::{color, keys, title_text, transform, transition};
use crate::app::Ui;
use crate::theme::{self, FIELD_H, Variant, c};

pub(super) fn body(ui: &Ui, id: ClipId, clip: &Clip) -> El {
    let Some(info) = ui.editor.clip_details(id) else {
        return col();
    };
    let inner = ui.details.width - 2.0 * PAD_X;
    let card_inner = inner - 2.0 * CARD_PAD;
    let value_w = card_inner - LABEL_W - GAP;

    let mut look = levels(ui, id, clip, info.has_sound, value_w);
    if clip.kind == ClipKind::Audio && info.has_source {
        look.extend(sound(ui, id, clip, value_w));
    }
    if clip.kind == ClipKind::Title {
        look.push(title_style(ui, clip));
    }
    look.extend(keys::navigation(ui, id, clip, card_inner));
    look.extend(links(id, clip, &info, card_inner));
    if info.can_caption {
        look.push(captions(ui, id));
    }

    let mut body = col()
        .w_full()
        .child(header(ui, clip))
        .child(el().h(16.0))
        .child(section(
            "Timing",
            timing(ui, id, clip, info.has_source, value_w),
        ));
    if !look.is_empty() {
        let title = match clip.kind {
            ClipKind::Video => "Picture & sound",
            ClipKind::Audio => "Sound",
            ClipKind::Title => "Look",
        };
        body = body.child(el().h(14.0)).child(section(title, look));
    }
    if clip.kind == ClipKind::Title {
        body = body
            .child(el().h(14.0))
            .child(title_text::card(ui, id, clip, value_w, card_inner));
    }
    if clip.kind == ClipKind::Video && ui.editor.timeline.clip_before(id).is_some() {
        body = body
            .child(el().h(14.0))
            .child(transition::card(ui, id, clip, value_w, card_inner));
    }
    if clip.kind == ClipKind::Video && info.has_source {
        body = body
            .child(el().h(14.0))
            .child(transform::card(ui, id, clip, value_w, card_inner))
            .child(el().h(14.0))
            .child(color::card(ui, id, clip, value_w, card_inner));
    }
    body.child(el().h(20.0)).child(actions(id, inner))
}

/// Where the clip sits, how long it plays and how fast.
fn timing(ui: &Ui, id: ClipId, clip: &Clip, has_source: bool, value_w: f32) -> Vec<El> {
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
    // A still has no source range and no speed: its length is all there is.
    let ranged = has_source && !clip.still();
    if ranged {
        timing.push(seconds_row(
            ui,
            "Source in",
            Field::SourceIn,
            clip,
            "Where playback starts inside the source file",
        ));
    }
    if ranged && clip.kind != ClipKind::Title {
        // Doubling and halving are equal steps, so 1x sits in the middle.
        timing.push(last_slider(slider_row(
            ui,
            id,
            "Speed",
            Field::Speed,
            clip.speed.log2() as f32,
            MIN_SPEED.log2() as f32..=MAX_SPEED.log2() as f32,
            value_w,
            speed_at,
            "How fast the clip plays, 0.25× to 4×; its sound keeps its pitch and its length follows",
        )));
    }
    timing
}

/// The Speed slider's handle at `position`, log2 of the speed, in the
/// hundredths the field shows. A step too small to change them still
/// moves one hundredth, so the arrow keys never stick at the slow end.
fn speed_at(editor: &mut Editor, id: ClipId, position: f32) {
    let Some(current) = editor.timeline.clip(id).map(|clip| clip.speed) else {
        return;
    };
    let wanted = 2f64.powf(f64::from(position));
    let mut speed = (wanted * 100.0).round() / 100.0;
    if speed == current && wanted != current {
        speed += if wanted > current { 0.01 } else { -0.01 };
    }
    editor.set_clip_speed(id, speed);
}

/// The Opacity and Volume sliders, for a clip that has a picture or sound.
fn levels(ui: &Ui, id: ClipId, clip: &Clip, has_sound: bool, value_w: f32) -> Vec<El> {
    let is_audio = clip.kind == ClipKind::Audio;
    let mut look = Vec::new();
    if !is_audio {
        // A title's opacity is one value; a picture's can change by key.
        let video = clip.kind == ClipKind::Video;
        let mut row = slider_row(
            ui,
            id,
            "Opacity",
            Field::Opacity,
            clip.opacity,
            0.0..=1.0,
            if video {
                value_w - keys::KEY_SPACE
            } else {
                value_w
            },
            Editor::set_clip_opacity,
            "Fades the picture against black",
        );
        if video {
            row = row.child(keys::button(
                ui,
                id,
                clip,
                &[Property::Opacity],
                "details-key-opacity",
                "opacity",
            ));
        }
        // A picture's Blend list, a shorter row, comes next.
        look.push(if has_sound && !video {
            row
        } else {
            last_slider(row)
        });
        if video {
            look.push(blend(ui, clip));
        }
    }
    if has_sound {
        let row = slider_row(
            ui,
            id,
            "Volume",
            Field::Volume,
            clip.volume,
            0.0..=MAX_VOLUME,
            value_w - keys::KEY_SPACE,
            Editor::set_clip_volume,
            "Clip gain; 100% is unity",
        )
        .child(keys::button(
            ui,
            id,
            clip,
            &[Property::Volume],
            "details-key-volume",
            "volume",
        ));
        look.push(if is_audio { row } else { last_slider(row) });
    }
    look
}

/// How a picture mixes with the lanes under it.
fn blend(ui: &Ui, clip: &Clip) -> El {
    let help =
        "How the picture mixes with the lanes under it: Screen drops black, Multiply drops white";
    prop_row(
        "Blend",
        help,
        select(
            "details-blend",
            clip.blend.label(),
            help,
            Select::Blend,
            ui.details.select == Some(Select::Blend),
        )
        .grow(1.0)
        .min_w(0.0),
    )
    .h(FIELD_H + 1.0)
    .pb(1.0)
}

/// An audio clip's fades and the switches that shape its sound.
fn sound(ui: &Ui, id: ClipId, clip: &Clip, value_w: f32) -> Vec<El> {
    vec![
        prop_row(
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
        ),
        checkbox(
            id,
            "details-smooth-fades",
            "Smooth fades",
            "Fade on an equal-power curve, which stays fuller through a crossfade",
            clip.smooth_fades,
            |clip| clip.smooth_fades = !clip.smooth_fades,
        ),
        checkbox(
            id,
            "details-duck",
            "Duck under speech",
            "Dip this sound while another clip is talking",
            clip.duck,
            |clip| clip.duck = !clip.duck,
        ),
        checkbox(
            id,
            "details-clean-voice",
            "Clean voice",
            "Cut low rumble and even out loud and quiet speech, in preview and export",
            clip.clean_voice,
            |clip| clip.clean_voice = !clip.clean_voice,
        ),
    ]
}

/// A title's own style.
fn title_style(ui: &Ui, clip: &Clip) -> El {
    let help = "Choose how this title or caption is drawn";
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
    .pb(1.0)
}

/// What ties a picture to its sound: the embedded-audio switch, the linked
/// partner's name, and the buttons that detach, unlink or tighten.
fn links(id: ClipId, clip: &Clip, info: &ClipDetails, card_inner: f32) -> Vec<El> {
    let mut look = Vec::new();
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
    look
}

/// The caption style and the button that generates captions in it.
fn captions(ui: &Ui, id: ClipId) -> El {
    let help = "Choose the style for generated captions";
    prop_row(
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
    )
}

/// Split and Delete, under the cards.
fn actions(id: ClipId, inner: f32) -> El {
    button_row(
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
    )
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

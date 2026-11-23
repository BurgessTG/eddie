//! The pick lists in Details: the selected title's own style and font,
//! the style generated captions get, a picture's blend mode, how it is
//! steadied and how its keyed values ease. Each is a closed field that opens a list under itself.

use openreel_engine::{BlendMode, ClipId, Ease, Stabilize, TitleFont, TitleStyle};
use rune::{Align, Cx, El, Family, Side, TextStyle};

use crate::app::Ui;
use crate::theme;

/// The list that is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    /// The selected title's own style.
    Title,
    /// The style generated captions get.
    Captions,
    /// The selected picture's blend mode.
    Blend,
    /// How hard the selected picture is steadied.
    Stabilize,
    /// The selected title's font.
    Font,
    /// How the selected clip's keyed values run on from the playhead.
    Ease,
}

/// A closed list: its choice and a down arrow.
pub(super) fn select(key: &'static str, choice: &str, help: &str, which: Select, open: bool) -> El {
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

pub(super) fn option_key(style: TitleStyle) -> String {
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
            Select::Blend | Select::Stabilize | Select::Font | Select::Ease => {}
        }
        ui.details.select = None;
    })
}

pub(super) fn blend_key(mode: BlendMode) -> String {
    format!("details-blend-{}", mode.as_str())
}

/// One blend mode in the open list.
fn blend_option(id: ClipId, mode: BlendMode, current: bool) -> El {
    theme::option(blend_key(mode), mode.label(), mode.help(), current).on_press(
        move |ui: &mut Ui| {
            ui.editor.set_clip_blend(id, mode);
            ui.details.select = None;
        },
    )
}

pub(super) fn stabilize_key(level: Option<Stabilize>) -> String {
    format!("details-stabilize-{}", level.map_or("off", Stabilize::name))
}

/// One stabilize level, or Off, in the open list.
fn stabilize_option(id: ClipId, level: Option<Stabilize>, current: bool) -> El {
    let (label, help) = match level {
        Some(level) => (level.label(), level.help()),
        None => ("Off", "Play the footage as shot"),
    };
    theme::option(stabilize_key(level), label, help, current).on_press(move |ui: &mut Ui| {
        ui.editor.stabilize_clip(id, level);
        ui.details.select = None;
    })
}

pub(super) fn font_key(font: TitleFont) -> String {
    format!("details-font-{}", font.as_str())
}

/// One font in the open list, its name drawn in it.
fn font_option(id: ClipId, font: TitleFont, current: bool) -> El {
    let look = TextStyle::new(15.0, 400.0).family(Family::Named(font.family()));
    theme::option_in(
        font_key(font),
        font.family(),
        look,
        &format!("Draw the text in {}", font.family()),
        current,
    )
    .on_press(move |ui: &mut Ui| {
        ui.editor.edit_title_text(id, |text| text.font = Some(font));
        ui.details.select = None;
    })
}

pub(super) fn ease_key(ease: Ease) -> String {
    format!("details-ease-{}", ease.as_str())
}

/// One ease in the open list.
fn ease_option(id: ClipId, ease: Ease, current: bool) -> El {
    theme::option(ease_key(ease), ease.label(), ease.help(), current).on_press(
        move |ui: &mut Ui| {
            ui.editor.set_ease_here(id, ease);
            ui.details.select = None;
        },
    )
}

/// The open list, hung under its field. A press outside closes it.
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
    let (anchor, gap, options): (_, _, Vec<El>) = match which {
        Select::Blend => (
            "details-blend",
            2.0,
            BlendMode::ALL
                .into_iter()
                .map(|mode| blend_option(id, mode, mode == clip.blend))
                .collect(),
        ),
        Select::Stabilize => {
            let current = match ui.editor.remake_pending(id) {
                Some(spec) => spec.stabilize,
                None => clip.remade.as_ref().and_then(|remade| remade.stabilize),
            };
            let levels = std::iter::once(None).chain(Stabilize::ALL.map(Some));
            (
                "details-stabilize",
                2.0,
                levels
                    .map(|level| stabilize_option(id, level, level == current))
                    .collect(),
            )
        }
        Select::Font => {
            let current = openreel_media::drawn_text(clip).font.unwrap_or_default();
            (
                "details-text-font",
                2.0,
                TitleFont::ALL
                    .into_iter()
                    .map(|font| font_option(id, font, font == current))
                    .collect(),
            )
        }
        Select::Ease => {
            let current = ui.editor.ease_here(id).flatten();
            (
                "details-ease",
                2.0,
                Ease::ALL
                    .into_iter()
                    .map(|ease| ease_option(id, ease, Some(ease) == current))
                    .collect(),
            )
        }
        Select::Title | Select::Captions => {
            let (anchor, current) = if which == Select::Title {
                ("details-style", clip.title_style)
            } else {
                ("details-caption-style", ui.editor.caption_style)
            };
            let styles = TitleStyle::ALL
                .into_iter()
                .map(|style| option(which, id, style, style == current));
            (anchor, 6.0, styles.collect())
        }
    };
    let close = |ui: &mut Ui| ui.details.select = None;
    overlays.push(rune_kit::scrim("details-select-scrim", close));
    // As wide as its field, wider only when a name needs it.
    let width = cx.layout_size(anchor).map_or(0.0, |size| size[0]);
    overlays.push(
        theme::popup("details-select")
            .min_w(width)
            .gap(gap)
            .children(options)
            .anchored(anchor, Side::Bottom, Align::Start, 0.0)
            .modal(true, close),
    );
}

//! The pick lists in Details: the selected title's own style, the one
//! generated captions get, and a picture's blend mode. Each is a closed
//! field that opens a list under itself.

use openreel_engine::{BlendMode, ClipId, TitleStyle};
use rune::{Align, Cx, El, Side};

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
            Select::Blend => {}
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

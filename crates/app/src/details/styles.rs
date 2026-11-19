//! The title-style lists: the selected title's own, and the one generated
//! captions get. Each is a closed field that opens a list under itself.

use openreel_engine::{ClipId, TitleStyle};
use rune::{Align, Cx, El, Side};

use crate::app::Ui;
use crate::theme;

/// The style list that is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    /// The selected title's own style.
    Title,
    /// The style generated captions get.
    Captions,
}

/// The closed style list: its choice and a down arrow.
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

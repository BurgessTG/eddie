//! Keyframe controls: the diamond beside each value that can change over a
//! clip, and the buttons that step the playhead between a clip's keys. The
//! keys themselves are `eddie_core::editor::keys`.

use openreel_engine::{Clip, ClipId, Property};
use rune::El;

use super::GAP;
use super::rows::button_row;
use crate::app::Ui;
use crate::icons;
use crate::theme::{self, Variant, c};

/// The room a row gives its key button: the button and the gap before it.
pub(super) const KEY_SPACE: f32 = 30.0 + GAP;

/// The diamond for `properties` (position keys across and down together):
/// filled when each has a key at the playhead, raised while any has keys.
/// It adds a key holding the value there, or takes the key there away.
pub(super) fn button(
    ui: &Ui,
    id: ClipId,
    clip: &Clip,
    properties: &'static [Property],
    key: &str,
    what: &str,
) -> El {
    let here = ui.editor.keyed_here(id, properties);
    let animated = properties
        .iter()
        .any(|&p| !clip.keyframes.keys(p).is_empty());
    let slack = 0.5 / ui.editor.timeline.fps;
    let on_clip = (clip.start - slack..=clip.end() + slack).contains(&ui.editor.playhead);
    let help = if !on_clip {
        format!("Move the playhead onto the clip to key its {what}")
    } else if here {
        format!("Remove the {what} key at the playhead")
    } else if animated {
        format!("Add a {what} key at the playhead")
    } else {
        format!(
            "Add a {what} key at the playhead; with two or more, the {what} changes from key to key"
        )
    };
    theme::icon_button(
        key,
        icons::key(here, c().foreground),
        &help,
        animated,
        on_clip,
    )
    .on_press(move |ui: &mut Ui| ui.editor.toggle_keys(id, properties))
}

/// Previous and next key, for a clip with keys.
pub(super) fn navigation(ui: &Ui, id: ClipId, clip: &Clip, width: f32) -> Option<El> {
    if clip.keyframes.is_empty() {
        return None;
    }
    let step = |forward: bool| {
        let (key, label, help) = if forward {
            (
                "details-next-key",
                "Next key",
                "Move the playhead to the clip's next key",
            )
        } else {
            (
                "details-previous-key",
                "Previous key",
                "Move the playhead to the clip's previous key",
            )
        };
        let enabled = ui.editor.key_beside(id, forward).is_some();
        theme::button(key, label, Variant::Secondary, help, enabled).on_press(move |ui: &mut Ui| {
            ui.editor.seek_key(id, forward);
        })
    };
    Some(button_row(vec![step(false), step(true)], width).py(2.0))
}

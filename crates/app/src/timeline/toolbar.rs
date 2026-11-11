//! The bar over the lanes: Split, Delete and the magnet on the left, the
//! zoom buttons and slider on the right.

use eddie_core::editor::viewport::MAX_ZOOM;
use eddie_core::{Action, shortcuts};
use rune::{Cursor, El, Phase, el, row, spacer};

use crate::app::Ui;
use crate::icons::{self, Tool};
use crate::theme::{self, c};

pub(super) const SLIDER_W: f32 = 110.0;

const SLIDER_HANDLE: f32 = 24.0;

/// Where the zoom handle's centre sits along the slider: logarithmic, so
/// each step of the way zooms by the same factor. The left end is `min`,
/// as far out as the timeline zooms.
pub(super) fn zoom_fraction(px_per_sec: f32, min: f32) -> f32 {
    ((px_per_sec / min).ln() / (MAX_ZOOM / min).ln()).clamp(0.0, 1.0)
}

pub(super) fn zoom_at(fraction: f32, min: f32) -> f32 {
    min * (MAX_ZOOM / min).powf(fraction.clamp(0.0, 1.0))
}

fn zoom_slider(ui: &Ui) -> El {
    let c = c();
    let zoom = ui.editor.viewport.px_per_sec;
    let min = ui.editor.viewport.min_zoom(ui.editor.timeline.end());
    let travel = SLIDER_W - SLIDER_HANDLE;
    let x = SLIDER_HANDLE / 2.0 + zoom_fraction(zoom, min) * travel;
    let rail = (SLIDER_HANDLE - 8.0) / 2.0;
    el().key("timeline-zoom")
        .size(SLIDER_W, SLIDER_HANDLE)
        .child(
            el().absolute(0.0, rail)
                .size(SLIDER_W, 8.0)
                .radius(4.0)
                .fill(c.secondary),
        )
        .child(
            // BRAND at 27% over the rail.
            el().absolute(0.0, rail)
                .size(x, 8.0)
                .radius(4.0)
                .fill(theme::hex("#2C4062")),
        )
        .child(
            el().absolute(x - SLIDER_HANDLE / 2.0, 0.0)
                .size(SLIDER_HANDLE, SLIDER_HANDLE)
                .radius(SLIDER_HANDLE / 2.0)
                .fill(c.secondary)
                .border(1.0, theme::hex("#E2E2E2")),
        )
        .instant_changes()
        .cursor(Cursor::Pointer)
        .help("Timeline zoom. Ctrl+scroll over the timeline also zooms.")
        .label("Timeline zoom")
        .range_value(f64::from(zoom), f64::from(min), f64::from(MAX_ZOOM))
        .on_step(|ui: &mut Ui, steps: i32, _| {
            ui.act(if steps > 0 {
                Action::ZoomIn
            } else {
                Action::ZoomOut
            });
        })
        .on_drag(move |ui: &mut Ui, drag, _| {
            if drag.phase == Phase::Up || drag.w <= SLIDER_HANDLE {
                return;
            }
            let fraction = (drag.x - SLIDER_HANDLE / 2.0) / (drag.w - SLIDER_HANDLE);
            // As the zoom keys do, keep the playhead where it is.
            let (end, playhead) = (ui.editor.timeline.end(), ui.editor.playhead);
            let port = &mut ui.editor.viewport;
            let zoom = zoom_at(fraction, port.min_zoom(end));
            port.zoom(zoom / port.px_per_sec, playhead, end);
        })
}

fn tool_button(key: &str, icon: Tool, active: bool, help: &str, action: Action) -> El {
    theme::icon_button(
        key,
        icons::tool(icon, c().foreground),
        &shortcuts::tip(help, action),
        active,
        true,
    )
    .on_press(move |ui: &mut Ui| ui.act(action))
}

pub(super) fn toolbar(ui: &Ui) -> El {
    let c = c();
    row()
        .key("timeline-toolbar")
        .w_full()
        .h(30.0)
        .gap(4.0)
        .center_y()
        .child(theme::panel_title("Timeline"))
        .child(el().w(6.0))
        .child(tool_button(
            "tool-magnet",
            Tool::Magnet,
            ui.editor.viewport.magnet,
            "Magnet: snap clip edges while dragging",
            Action::ToggleMagnet,
        ))
        .child(
            row()
                .size(13.0, 18.0)
                .center()
                .child(el().w(1.0).h_full().fill(c.border_strong)),
        )
        .child(
            theme::letter_button(
                "tool-title",
                "T",
                &shortcuts::tip("Add a title clip at the playhead", Action::AddTitle),
                false,
                true,
            )
            .on_press(|ui: &mut Ui| ui.act(Action::AddTitle)),
        )
        .child(tool_button(
            "tool-split",
            Tool::Split,
            false,
            "Split the clip under the playhead",
            Action::SplitSelected,
        ))
        .child(tool_button(
            "tool-delete",
            Tool::Delete,
            false,
            "Delete the selected clip",
            Action::DeleteSelected,
        ))
        .child(spacer())
        .child(tool_button(
            "tool-zoom-out",
            Tool::ZoomOut,
            false,
            "Zoom out",
            Action::ZoomOut,
        ))
        .child(row().pl(1.0).child(zoom_slider(ui)))
        .child(tool_button(
            "tool-zoom-in",
            Tool::ZoomIn,
            false,
            "Zoom in",
            Action::ZoomIn,
        ))
}

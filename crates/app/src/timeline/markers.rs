//! Markers in the window: a flag on the ruler for each, and the popup that
//! names, recolours and deletes one. Clicks, drags and the snap are
//! `eddie_core`'s `lanes`; this module draws them and edits a marker.

use eddie_core::Action;
use eddie_core::editor::lanes::{self, HEADER_W, RULER_H};
use openreel_engine::{MarkerColor, MarkerId, format_timecode};
use rune::{Align, Color, Cursor, El, Key, LineCap, Shape, Side, TextEdit, el, row, spacer, text};

use crate::app::Ui;
use crate::theme::{self, Variant, c};

const W: f32 = 264.0;
/// A flag's pennant, right of its pole.
const FLAG_W: f32 = 9.0;
const SWATCH: f32 = 22.0;

/// The marker the popup edits, if it is open, and its note as typed. The
/// box outlives the popup: Rune still reaches it in the frame that closes it.
#[derive(Default)]
pub struct MarkerEdit {
    pub id: Option<MarkerId>,
    pub name: TextEdit,
}

impl MarkerEdit {
    pub fn typing(&self) -> bool {
        self.id.is_some() && self.name.focused
    }
}

/// A marker's colour on screen.
pub fn color(color: MarkerColor) -> Color {
    theme::hex(match color {
        MarkerColor::Yellow => "#FACC15",
        MarkerColor::Orange => "#FB923C",
        MarkerColor::Red => "#F87171",
        MarkerColor::Pink => "#F472B6",
        MarkerColor::Purple => "#C084FC",
        MarkerColor::Blue => "#60A5FA",
        MarkerColor::Cyan => "#22D3EE",
        MarkerColor::Green => "#4ADE80",
    })
}

fn label(color: MarkerColor) -> &'static str {
    match color {
        MarkerColor::Yellow => "Yellow",
        MarkerColor::Orange => "Orange",
        MarkerColor::Red => "Red",
        MarkerColor::Pink => "Pink",
        MarkerColor::Purple => "Purple",
        MarkerColor::Blue => "Blue",
        MarkerColor::Cyan => "Cyan",
        MarkerColor::Green => "Green",
    }
}

/// M, Alt+M: a marker at the playhead. On a marker already there, M
/// opens that marker's popup instead, so a note is a keypress away.
pub fn add(ui: &mut Ui, action: Action) {
    let existing = ui
        .editor
        .timeline
        .marker_at(ui.editor.playhead)
        .map(|marker| marker.id);
    let id = ui
        .editor
        .add_marker_here(action == Action::AddMarkerNewColor);
    if existing == Some(id) {
        open(ui, id);
    }
}

/// Open the popup for marker `id`, its note ready to type over.
pub fn open(ui: &mut Ui, id: MarkerId) {
    let Some(marker) = ui.editor.timeline.marker(id) else {
        return;
    };
    let mut name = TextEdit::new(&marker.name);
    let end = marker.name.len();
    name.set_selection(0, end);
    name.focused = true;
    ui.marker_edit = MarkerEdit { id: Some(id), name };
}

pub fn close(ui: &mut Ui) {
    ui.marker_edit.id = None;
    ui.marker_edit.name.focused = false;
}

/// Once per editor frame: the note goes to the marker as it is typed, and
/// an undo shows in the box. The popup closes when its marker is gone.
pub fn sync(ui: &mut Ui) {
    let edit = &mut ui.marker_edit;
    let Some(id) = edit.id else {
        return;
    };
    let Some(marker) = ui.editor.timeline.marker_mut(id) else {
        close(ui);
        return;
    };
    if edit.name.focused {
        let typed = edit.name.text.replace(['\n', '\r'], " ");
        if marker.name != typed {
            marker.name = typed;
        }
    } else if edit.name.text.as_str() != marker.name {
        edit.name.replace_text(marker.name.clone());
    }
}

/// What the lanes' tooltip says over a marker's flag.
pub fn help(ui: &Ui, id: MarkerId) -> String {
    let Some(marker) = ui.editor.timeline.marker(id) else {
        return String::new();
    };
    let when = format_timecode(marker.time, ui.editor.timeline.fps);
    let what = if marker.name.is_empty() {
        format!("Marker {}", marker.id)
    } else {
        marker.name.clone()
    };
    format!("{what} · {when}. Drag to move it; double-click or right-click to edit it")
}

/// A flag on the ruler for each marker, `x` its pole. A marker that is
/// hovered, dragged or being edited also draws a line down the lanes, so
/// a clip edge can be matched to it.
pub(super) fn flags(ui: &Ui, width: f32, height: f32, hovered: Option<MarkerId>) -> Vec<El> {
    let port = &ui.editor.viewport;
    let lit = [
        hovered,
        ui.lanes.gesture.dragged_marker(),
        ui.marker_edit.id,
    ];
    // Under the ruler's timecodes (drawn 3 to about 16 points down), so a
    // flag on a labelled second never hides its label.
    let top = RULER_H - 9.0;
    ui.editor
        .timeline
        .markers_in_order()
        .into_iter()
        .filter_map(|marker| {
            let x = lanes::x_at(port, 0.0, marker.time).floor() + 0.5;
            if !(-FLAG_W..=width).contains(&x) {
                return None;
            }
            let fill = color(marker.color);
            let down = if lit.contains(&Some(marker.id)) {
                height
            } else {
                RULER_H
            };
            let mut shape = Shape::new()
                .view_box(FLAG_W + 1.0, height)
                .line([0.75, top], [0.75, down], 1.5, LineCap::Butt, fill)
                .fill(
                    vec![
                        [0.0, top],
                        [FLAG_W, top],
                        [FLAG_W - 2.5, top + 3.5],
                        [FLAG_W, top + 7.0],
                        [0.0, top + 7.0],
                    ],
                    fill,
                );
            if down > RULER_H {
                // Fainter down the lanes than on the ruler.
                shape = shape.line(
                    [0.75, RULER_H],
                    [0.75, height],
                    1.5,
                    LineCap::Butt,
                    Color::BLACK.alpha(0.35),
                );
            }
            Some(
                el().key(format!("marker-{}", marker.id))
                    .absolute(x - 0.75, 0.0)
                    .size(FLAG_W + 1.0, height)
                    .shape(shape),
            )
        })
        .collect()
}

/// One colour to pick, ringed when it is the marker's.
fn swatch(id: MarkerId, choice: MarkerColor, current: bool) -> El {
    let c = c();
    let name = label(choice);
    let swatch = el()
        .key(format!("marker-color-{}", choice.as_str()))
        .size(SWATCH, SWATCH)
        .radius(SWATCH / 2.0)
        .fill(color(choice))
        .cursor(Cursor::Pointer)
        .role("option")
        .label(name)
        .help(format!("Make this marker {}", name.to_lowercase()))
        .on_press(move |ui: &mut Ui| {
            if let Some(marker) = ui.editor.timeline.marker_mut(id) {
                marker.color = choice;
            }
        });
    if current {
        swatch.outline(2.0, 1.5, c.foreground)
    } else {
        swatch.hover(c.secondary)
    }
}

/// The popup for the marker being edited, hung under its flag: its note,
/// its colour and Delete. A press outside or Esc closes it.
pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    let edit = &ui.marker_edit;
    let (Some(id), Some([left, top, width, _])) = (edit.id, ui.timeline_rect) else {
        return;
    };
    let Some(marker) = ui.editor.timeline.marker(id) else {
        return;
    };
    let c = c();
    let lanes_w = (width - HEADER_W).max(0.0);
    let x = lanes::x_at(&ui.editor.viewport, 0.0, marker.time).clamp(0.0, lanes_w);
    overlays.push(rune_kit::scrim("marker-scrim", close));
    overlays.push(
        el().key("marker-point")
            .absolute(left + HEADER_W + x, top + RULER_H)
            .size(0.0, 0.0),
    );
    let name = rune::text_input(&edit.name, theme::BODY)
        .selection_color(theme::selection())
        .key("marker-name")
        .single_line()
        .w_full()
        .px(8.0)
        .py(6.0)
        .radius(theme::R_MD)
        .fill(c.input)
        .color(c.foreground)
        .placeholder("Add a note", c.muted_fg)
        .focus_within_ring(1.0, -1.0, c.brand)
        .help("What this marker is for, such as \"B-roll here\"")
        .on_edit(|ui: &mut Ui| &mut ui.marker_edit.name)
        .on_submit(|ui: &mut Ui, tx| {
            sync(ui);
            close(ui);
            tx.blur();
        })
        .on_input_key(|ui: &mut Ui, event, tx| {
            if matches!(event.key, Key::Escape) {
                sync(ui);
                close(ui);
                tx.blur();
                true
            } else {
                false
            }
        });
    let swatches = MarkerColor::ALL
        .into_iter()
        .fold(row().w_full().gap(8.0), |row, choice| {
            row.child(swatch(id, choice, choice == marker.color))
        });
    let delete = theme::button(
        "marker-delete",
        "Delete",
        Variant::Destructive,
        "Delete this marker",
        true,
    )
    .on_press(move |ui: &mut Ui| {
        close(ui);
        ui.editor.delete_marker(id);
    });
    let card = theme::card()
        .key("marker-editor")
        .w(W)
        .gap(12.0)
        .shadow(4.0, 16.0, Color::BLACK.alpha(120.0 / 255.0))
        .role("dialog")
        .label("Marker")
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(theme::panel_title(&format!("Marker {id}")))
                .child(
                    text(format_timecode(marker.time, ui.editor.timeline.fps))
                        .key("marker-time")
                        .font(theme::mono(11.0))
                        .color(c.muted_fg)
                        .single_line(),
                )
                .child(spacer())
                .child(theme::close_button("marker-close", "Close (Esc)").on_press(close)),
        )
        .child(name)
        .child(swatches)
        .child(row().w_full().child(spacer()).child(delete))
        .anchored("marker-point", Side::Bottom, Align::Start, 6.0)
        .modal(true, close);
    overlays.push(card);
}

#[cfg(test)]
mod tests;

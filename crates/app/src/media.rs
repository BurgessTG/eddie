//! The media bin: an image grid of imported files, with drag to the
//! timeline and the Curate controls.
//!
//! `tile` is one file in the grid and `curator` the card under it.

mod curator;
mod tile;

use eddie_core::Action;
use eddie_core::editor::media::{AssetState, BinAsset};
use openreel_engine::ClipKind;
use rune::{Color, Cursor, El, Phase, ScrollSpec, SurfaceFit, col, el, row, spacer, text};
use rune_kit::{ScrollBar, ScrollBarStyle};

pub(crate) use tile::spinner;
use tile::{layer, tile};

use crate::app::Ui;
use crate::theme::{self, Variant, c};
use crate::{menus, timeline};

pub const DEFAULT_W: f32 = 280.0;
const MIN_W: f32 = 220.0;
const MAX_W: f32 = 420.0;
/// The panel's side padding; the right border is drawn inside it.
const PAD_X: f32 = 14.0;
const GAP: f32 = 8.0;
const MIN_TILE_W: f32 = 112.0;
/// Pointer travel that turns a press on a tile into a drag.
const DRAG_START: f32 = 6.0;

/// A bin tile on its way to the timeline.
pub struct BinDrag {
    pub path: String,
    /// The pointer in window points.
    pub at: [f32; 2],
    /// The tile's size, which the picture under the pointer keeps.
    pub size: [f32; 2],
}

/// The right-click menu of one bin tile, and where it opened.
pub struct BinMenu {
    pub path: String,
    pub at: [f32; 2],
}

/// The grid for a panel's content width: columns, then a tile's size.
/// Tiles stretch to fill the row so the grid has even edges.
fn grid(width: f32) -> (usize, f32, f32) {
    let columns = (((width + GAP) / (MIN_TILE_W + GAP)).floor() as usize).max(1);
    let tile_w = ((width - GAP * (columns - 1) as f32) / columns as f32)
        .floor()
        .max(40.0);
    (columns, tile_w, (tile_w * 9.0 / 16.0).round())
}

fn header(ui: &Ui) -> El {
    let c = c();
    let mut header = row()
        .h(28.0)
        .gap(8.0)
        .center_y()
        .child(theme::panel_title("Media"));
    if !ui.editor.assets.is_empty() {
        header = header.child(
            text(ui.editor.assets.len().to_string())
                .font(theme::SMALL)
                .color(c.subtle_fg)
                .single_line(),
        );
    }
    header.child(spacer()).child(
        theme::button(
            "bin-import",
            "Import",
            Variant::Ghost,
            "Import video or audio files",
            true,
        )
        .on_press(|ui: &mut Ui| ui.act(Action::Import)),
    )
}

/// The left dock. Its right edge drags to resize it.
pub fn panel(ui: &Ui) -> El {
    let c = c();
    let editor = &ui.editor;
    let width = ui.media_w - PAD_X * 2.0;
    let mut content = col()
        .w(ui.media_w)
        .h_full()
        .px(PAD_X)
        .py(12.0)
        .child(header(ui));

    if editor.assets.is_empty() {
        content = content
            .child(el().h(22.0))
            .child(empty())
            .child(el().h(16.0))
            .child(
                theme::button(
                    "bin-import-media",
                    "Import media",
                    Variant::Primary,
                    "Import files, or drop them onto this window",
                    true,
                )
                .w_full()
                .on_press(|ui: &mut Ui| ui.act(Action::Import)),
            );
    } else {
        content = content
            .child(el().h(14.0))
            .child(search(ui))
            .child(el().h(16.0))
            .child(tiles(ui, width));
        if editor
            .assets
            .iter()
            .any(|asset| asset.state == AssetState::Ready)
        {
            content = content.child(el().h(12.0)).child(curator::card(ui, width));
        }
    }

    el().key("media-bin")
        .w(ui.media_w)
        .h_full()
        .fill(c.surface)
        .child(content)
        .child(
            el().absolute(ui.media_w - 1.0, 0.0)
                .w(1.0)
                .h_full()
                .fill(c.border),
        )
        .child(
            el().key("media-bin-resize")
                .absolute(ui.media_w - 4.0, 0.0)
                .w(7.0)
                .h_full()
                .cursor(Cursor::ResizeColumn)
                .help("Drag to resize the media bin")
                .on_drag(|ui: &mut Ui, drag, _| {
                    if drag.phase == Phase::Down {
                        drag.data = ui.media_w;
                    } else {
                        ui.media_w = (drag.data + drag.dx).round().clamp(MIN_W, MAX_W);
                    }
                }),
        )
}

/// What an empty bin shows where the grid will be.
fn empty() -> El {
    let c = c();
    col()
        .key("bin-empty")
        .w_full()
        .h(132.0)
        .center()
        .gap(4.0)
        .fill(c.card)
        .border(1.0, c.border)
        .radius(theme::R_LG)
        .child(
            text("No media yet")
                .font(theme::BODY)
                .color(c.foreground)
                .single_line(),
        )
        .child(
            text("Drop files here")
                .font(theme::SMALL)
                .color(c.muted_fg)
                .single_line(),
        )
}

/// The box that filters the grid by file name.
fn search(ui: &Ui) -> El {
    let c = c();
    rune::text_input(&ui.search, theme::BODY)
        .selection_color(theme::selection())
        .key("bin-search")
        .single_line()
        .w_full()
        .px(8.0)
        .py(6.0)
        .radius(theme::R_MD)
        .fill(c.input)
        .color(c.foreground)
        .placeholder("Search media…", c.muted_fg)
        .focus_within_ring(1.0, -1.0, c.brand)
        .help("Filter media by name")
        .on_edit(|ui: &mut Ui| &mut ui.search)
}

/// The scrolling grid of the files the search box lets through.
fn tiles(ui: &Ui, width: f32) -> El {
    let c = c();
    let query = ui.search.text.to_lowercase();
    let shown: Vec<&BinAsset> = ui
        .editor
        .assets
        .iter()
        .filter(|asset| query.is_empty() || asset.info.name.to_lowercase().contains(&query))
        .collect();
    let (columns, tile_w, tile_h) = grid(width);
    let rows = shown.chunks(columns).map(|assets| {
        row().gap(GAP).children(
            assets
                .iter()
                .map(|asset| tile(ui, asset, tile_w, tile_h))
                .collect::<Vec<_>>(),
        )
    });
    col()
        .key("bin-grid")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(col().gap(14.0).children(rows.collect::<Vec<_>>()))
        .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside())
}

/// What floats over the window for the bin: the dragged tile with the lane
/// it would land on, and a tile's right-click menu.
pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    let c = c();
    if let Some(drag) = &ui.bin_drag {
        // The lane of the file's kind it would land on; none when it would
        // get a new lane.
        let lands = timeline::drop_target(ui, drag.at).and_then(|(row, at)| {
            let info = &ui.editor.asset(&drag.path)?.info;
            let kind = if info.has_video {
                ClipKind::Video
            } else {
                ClipKind::Audio
            };
            let timeline = &ui.editor.timeline;
            let track = timeline.drop_lane(kind, row, at, at + info.duration)?;
            Some(timeline.row(kind, track))
        });
        if let Some([x, y, w, h]) = lands.and_then(|row| timeline::lane_rect(ui, row)) {
            overlays.push(
                el().key("bin-drop-lane")
                    .absolute(x, y)
                    .size(w, h)
                    .radius(theme::R_MD)
                    .fill(Color::WHITE.alpha(18.0 / 255.0))
                    .border(1.5, c.ring)
                    .inert(),
            );
        }
        let [w, h] = drag.size;
        let mut ghost = el()
            .key("bin-drag-ghost")
            .absolute(drag.at[0] + 12.0, drag.at[1] + 12.0)
            .size(w, h)
            .radius(theme::R_MD)
            .fill(c.card)
            .inert();
        if let Some(thumb) = ui.thumbs(&drag.path).first() {
            ghost = ghost.child(
                layer(w, h, theme::R_MD)
                    .pixels_fit(&thumb.pixels, SurfaceFit::Fill)
                    .opacity(220.0 / 255.0),
            );
        }
        overlays.push(ghost.child(layer(w, h, theme::R_MD).border(2.0, c.ring)));
    }
    if let Some(menu) = &ui.bin_menu {
        let close = |ui: &mut Ui| ui.bin_menu = None;
        let path = menu.path.clone();
        overlays.push(rune_kit::scrim("bin-menu-scrim", close));
        overlays.push(
            menus::frame("bin-menu")
                .absolute(menu.at[0], menu.at[1])
                .child(
                    menus::destructive_row(
                        "bin-menu-remove",
                        "Remove from bin",
                        "Take this file out of the bin; the file stays on disk",
                    )
                    .on_press(move |ui: &mut Ui| {
                        ui.bin_menu = None;
                        ui.editor.remove_asset(&path);
                    }),
                )
                .modal(true, close),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_panel_shows_two_columns_of_16_by_9_tiles() {
        assert_eq!(grid(DEFAULT_W - PAD_X * 2.0), (2, 122.0, 69.0));
        // A panel too narrow for one minimum tile still shows one.
        assert_eq!(grid(100.0), (1, 100.0, 56.0));
        assert_eq!(grid(MAX_W - PAD_X * 2.0), (3, 125.0, 70.0));
    }
}

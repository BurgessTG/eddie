//! The media bin: an image grid of imported files, with drag to the
//! timeline and the Curate controls.

use eddie_core::Action;
use eddie_core::editor::media::{AssetState, BinAsset, asset_meta, count};
use eddie_core::editor::sweep::{ReviewStatus, SweepStatus};
use rune::{
    Color, Cursor, El, LineCap, Phase, PointerPress, ScrollSpec, Shape, SurfaceFit, col, el, row,
    spacer, text,
};
use rune_kit::{ScrollBar, ScrollBarStyle};

use crate::app::{Thumb, Ui};
use crate::theme::{self, Variant, c};
use crate::{icons, menus, timeline};

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

/// Whole-file waveform in the timeline's audio colours. Linear scale: a
/// whole-file overview of loud music would fill the tile solid under the
/// timeline's square-root scale.
fn peaks_shape(w: f32, h: f32, peaks: &[f32]) -> Shape {
    let mut shape = Shape::new().view_box(w, h);
    if peaks.is_empty() {
        return shape;
    }
    // CLIP_AUDIO × 2.6.
    let color = theme::hex("#72DA9C");
    let (left, right) = (10.0, w - 10.0);
    let inner_h = h - h * 0.44;
    let mid = h / 2.0;
    let slice =
        |x: f32| (((x - left) / (right - left).max(1.0)) * peaks.len() as f32).max(0.0) as usize;
    let mut x = left;
    while x <= right {
        // Each bar shows the loudest peak in its slice of the file.
        let start = slice(x).min(peaks.len() - 1);
        let end = slice(x + 3.0).clamp(start + 1, peaks.len());
        let peak = peaks[start..end].iter().copied().fold(0.0_f32, f32::max);
        let amp = (peak.clamp(0.0, 1.0) * inner_h / 2.0).max(1.0);
        shape = shape.line([x, mid - amp], [x, mid + amp], 1.5, LineCap::Butt, color);
        x += 3.0;
    }
    shape
}

/// A layer covering a `w`×`h` box with rounded corners; input passes through.
fn layer(w: f32, h: f32, radius: f32) -> El {
    el().absolute(0.0, 0.0).size(w, h).radius(radius).inert()
}

/// An asset's first still or its waveform, filling a `w`×`h` box.
fn preview(
    mut frame: El,
    thumb: Option<&Thumb>,
    asset: &BinAsset,
    w: f32,
    h: f32,
    radius: f32,
) -> El {
    if let Some(thumb) = thumb {
        frame = frame.child(layer(w, h, radius).pixels_fit(&thumb.pixels, SurfaceFit::Fill));
    } else if !asset.peaks.is_empty() {
        frame = frame.child(
            // CLIP_AUDIO at 55% over the card.
            layer(w, h, radius)
                .fill(theme::hex("#23392D"))
                .shape(peaks_shape(w, h, &asset.peaks)),
        );
    }
    frame
}

pub(crate) fn spinner(size: f32, turn: f32) -> El {
    el().size(size, size)
        .shape(icons::spinner(size, turn, c().foreground))
}

fn tile(ui: &Ui, asset: &BinAsset, w: f32, h: f32) -> El {
    let c = c();
    let editor = &ui.editor;
    let path = asset.info.path.clone();
    let ready = asset.state == AssetState::Ready;
    let clashes = editor.clashes_with_format(&asset.info);
    let selected = editor.selected_asset.as_deref() == Some(path.as_str());
    let thumb = ui.thumbs(&path).first();
    let has_preview = thumb.is_some() || !asset.peaks.is_empty();
    // Placeholders read as a grey card until their thumbnails or peaks
    // arrive.
    let mut tile = el()
        .key(format!("bin-tile:{path}"))
        .size(w, h)
        .radius(theme::R_MD)
        .fill(if ready { c.card } else { c.secondary })
        .cursor(Cursor::Pointer)
        .hover_group();
    tile = preview(tile, thumb, asset, w, h, theme::R_MD);
    if thumb.is_some() && (!ready || clashes) {
        // Dimmed: importing, or the other format.
        tile = tile.child(layer(w, h, theme::R_MD).fill(Color::BLACK.alpha(150.0 / 255.0)));
    }
    if !has_preview && ready && asset.info.has_video {
        tile = tile.child(
            layer(w, h, theme::R_MD).shape(Shape::new().view_box(w, h).fill(
                vec![
                    [w / 2.0 - 7.0, h / 2.0 - 10.0],
                    [w / 2.0 + 10.0, h / 2.0],
                    [w / 2.0 - 7.0, h / 2.0 + 10.0],
                ],
                Color::WHITE.alpha(45.0 / 255.0),
            )),
        );
    }
    if !ready {
        if has_preview {
            tile = tile.child(layer(w, h, theme::R_MD).fill(Color::BLACK.alpha(100.0 / 255.0)));
        }
        tile = tile.child(
            spinner(20.0, ui.turn())
                .absolute((w - 20.0) / 2.0, (h - 20.0) / 2.0)
                .inert(),
        );
    } else if asset.sweep.status == SweepStatus::Running {
        // Curating: a corner spinner, so the thumbnail stays readable.
        tile = tile.child(
            el().absolute(w - 24.0, 2.0)
                .size(22.0, 22.0)
                .radius(11.0)
                .fill(Color::BLACK.alpha(170.0 / 255.0))
                .inert()
                .child(spinner(18.0, ui.turn()).absolute(2.0, 2.0)),
        );
    }
    tile = tile.child(if selected {
        layer(w, h, theme::R_MD).border(2.0, c.brand)
    } else {
        layer(w, h, theme::R_MD)
            .border(1.0, Color::WHITE.alpha(70.0 / 255.0))
            .group_opacity(0.0, 1.0)
    });

    let meta = asset_meta(asset);
    let name = &asset.info.name;
    let hover = if clashes {
        format!(
            "{name}\n{meta}\n{path}\nIts orientation doesn't match the {} project, so it isn't added.",
            editor.timeline.orientation.as_str()
        )
    } else if ready {
        format!("{name}\n{meta}\n{path}\nDouble-click or drag onto the timeline to add.")
    } else {
        format!("{name}\n{meta}\nImporting…")
    };
    let (drag_path, menu_path) = (path.clone(), path);
    tile.help(format!("{hover}\nRight-click to remove it."))
        .on_drag(move |ui: &mut Ui, drag, _| match drag.phase {
            Phase::Down => {}
            Phase::Move => {
                // Carry a grid-size preview at the cursor so the drop
                // reads clearly, plus the lane it would land on.
                if ui.bin_drag.is_some() || drag.dx.hypot(drag.dy) >= DRAG_START {
                    ui.bin_drag = Some(BinDrag {
                        path: drag_path.clone(),
                        at: drag.window_point(),
                        size: [w, h],
                    });
                }
            }
            Phase::Up => {
                let dragged = ui.bin_drag.take().is_some();
                if drag.cancelled() {
                    return;
                }
                if dragged {
                    if let Some((track, time)) = timeline::drop_target(ui, drag.window_point()) {
                        ui.editor
                            .add_asset_to_timeline(&drag_path, Some(track), time);
                    }
                    return;
                }
                ui.editor.selected_asset = Some(drag_path.clone());
                if drag.clicks == 2 {
                    let playhead = ui.editor.playhead;
                    ui.editor.add_asset_to_timeline(&drag_path, None, playhead);
                }
            }
        })
        .on_secondary_press(move |ui: &mut Ui, press: PointerPress, _| {
            ui.bin_menu = Some(BinMenu {
                path: menu_path.clone(),
                at: [press.x, press.y],
            });
        })
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

/// What the Curator card says about the selected asset.
fn summary(ui: &Ui, asset: &BinAsset) -> String {
    let sweep = &asset.sweep;
    let needs_review = sweep
        .candidates
        .iter()
        .filter(|clip| clip.review.status == ReviewStatus::NeedsReview)
        .count();
    match sweep.status {
        SweepStatus::NotStarted if asset.state != AssetState::Ready => "Importing…".to_owned(),
        SweepStatus::NotStarted => "Not curated yet".to_owned(),
        SweepStatus::Running if ui.editor.sweep_queue.contains(&asset.info.path) => {
            "Queued".to_owned()
        }
        SweepStatus::Running => sweep.note.clone(),
        SweepStatus::Failed => "Analysis failed".to_owned(),
        _ if needs_review > 0 => format!(
            "{} · {needs_review} need review",
            count(sweep.candidates.len(), "clip")
        ),
        _ => format!(
            "{} · {}",
            count(sweep.candidates.len(), "clip"),
            count(sweep.scene_clips.len(), "scene")
        ),
    }
}

const GALLERY_HELP: &str = "Pause playback and search, filter or sort curated clips";

/// The Curate controls for the selected asset, pinned under the grid.
fn curator_card(ui: &Ui, width: f32) -> El {
    let c = c();
    let editor = &ui.editor;
    let running = editor
        .assets
        .iter()
        .filter(|asset| asset.sweep.status == SweepStatus::Running)
        .count();
    let mut title = row()
        .h(28.0)
        .center_y()
        .child(theme::panel_title("Curator"))
        .child(spacer());
    if running > 0 {
        title = title.child(
            row()
                .key("curator-running")
                .help(format!("Curating {}", count(running, "video")))
                .child(
                    el().size(13.0, 13.0)
                        .shape(icons::spinner(13.0, ui.turn(), c.foreground)),
                ),
        );
    }
    let mut card = theme::card()
        .key("curator-card")
        .w(width)
        .gap(8.0)
        .child(title);
    let selected = editor
        .selected_asset
        .as_deref()
        .and_then(|path| editor.asset(path));
    let Some(asset) = selected else {
        return card
            .child(theme::caption("Select a video, then press Curate."))
            .child(
                theme::button(
                    "curator-browse",
                    "Browse gallery",
                    Variant::Secondary,
                    GALLERY_HELP,
                    true,
                )
                .w_full()
                .on_press(|ui: &mut Ui| ui.editor.open_curator_gallery()),
            );
    };
    let path = asset.info.path.clone();
    let summary = summary(ui, asset);
    let note = if asset.sweep.note.is_empty() {
        summary.clone()
    } else {
        asset.sweep.note.clone()
    };
    // The selected source, so the card reads at a glance.
    let thumb = preview(
        el().size(56.0, 32.0).radius(theme::R_SM).fill(c.secondary),
        ui.thumbs(&path).first(),
        asset,
        56.0,
        32.0,
        theme::R_SM,
    );
    card = card.child(
        row().h(33.0).gap(10.0).child(thumb).child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(2.0)
                .child(
                    text(asset.info.name.clone())
                        .font(theme::sized(12.5))
                        .color(c.foreground)
                        .single_line()
                        .truncate()
                        .help(path.clone()),
                )
                .child(theme::caption(summary).single_line().truncate().help(note)),
        ),
    );

    let inner = width - 26.0;
    let half = ((inner - 6.0) / 2.0).floor();
    let curate = match asset.sweep.status {
        SweepStatus::NotStarted => Some((
            "Curate",
            Variant::Primary,
            "Find every shot, transcribe speech and rank usable ranges, on this computer unless Speech uses a cloud provider",
        )),
        SweepStatus::Running => None,
        SweepStatus::Ready => Some((
            "Re-curate",
            Variant::Secondary,
            "Analyze again, for example after changing the speech provider",
        )),
        SweepStatus::Failed => Some((
            "Retry",
            Variant::Secondary,
            "Try analyzing this video again",
        )),
    };
    let mut buttons = row().h(30.0).gap(6.0).center_y();
    if let Some((label, variant, tooltip)) = curate {
        // Curating ignores assets still importing; say so instead of
        // taking a silent click.
        let ready = asset.state == AssetState::Ready;
        let button = theme::button(
            "curator-curate",
            label,
            variant,
            if ready {
                tooltip
            } else {
                "Available once import finishes"
            },
            ready,
        )
        .w(half);
        buttons = buttons.child(if ready {
            let path = path.clone();
            button.on_press(move |ui: &mut Ui| ui.editor.curate_asset(path.clone()))
        } else {
            button
        });
    }
    card = card.child(
        buttons.child(
            theme::button(
                "curator-gallery",
                "Gallery",
                Variant::Secondary,
                GALLERY_HELP,
                true,
            )
            .grow(1.0)
            .on_press(|ui: &mut Ui| ui.editor.open_curator_gallery()),
        ),
    );

    let uncurated = editor
        .assets
        .iter()
        .filter(|asset| {
            asset.state == AssetState::Ready && asset.sweep.status == SweepStatus::NotStarted
        })
        .count();
    if uncurated > 1 || (uncurated == 1 && asset.sweep.status != SweepStatus::NotStarted) {
        card = card.child(
            theme::button(
                "curator-all-new",
                &format!("Curate all new ({uncurated})"),
                Variant::Ghost,
                "Curate every imported video that has not been curated",
                true,
            )
            .w_full()
            .on_press(|ui: &mut Ui| ui.editor.curate_all_new()),
        );
    }
    card
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
            .child(
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
                    ),
            )
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
        let query = ui.search.text.to_lowercase();
        let shown: Vec<&BinAsset> = editor
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
        let search = rune::text_input(&ui.search, theme::BODY)
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
            .on_edit(|ui: &mut Ui| &mut ui.search);
        content = content
            .child(el().h(14.0))
            .child(search)
            .child(el().h(16.0))
            .child(
                col()
                    .key("bin-grid")
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .scroll(ScrollSpec {
                        stick_bottom: false,
                    })
                    .child(col().gap(14.0).children(rows.collect::<Vec<_>>()))
                    .scroll_bar_styled(
                        ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside(),
                    ),
            );
        if editor
            .assets
            .iter()
            .any(|asset| asset.state == AssetState::Ready)
        {
            content = content.child(el().h(12.0)).child(curator_card(ui, width));
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

/// What floats over the window for the bin: the dragged tile with the lane
/// it would land on, and a tile's right-click menu.
pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    let c = c();
    if let Some(drag) = &ui.bin_drag {
        if let Some((track, _)) = timeline::drop_target(ui, drag.at) {
            let lane = track.min(ui.editor.timeline.track_labels.len());
            if let Some([x, y, w, h]) = timeline::lane_rect(ui, lane) {
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

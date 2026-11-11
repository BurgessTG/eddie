//! One bin tile: its still or waveform, its tooltip, and the drag that
//! carries it to the timeline.

use eddie_core::editor::media::{AssetState, BinAsset, asset_meta};
use eddie_core::editor::sweep::SweepStatus;
use rune::{Color, Cursor, El, LineCap, Phase, PointerPress, Shape, SurfaceFit, el};

use super::{BinDrag, BinMenu, DRAG_START};
use crate::app::{Thumb, Ui};
use crate::theme::{self, c};
use crate::{icons, timeline};

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
pub(super) fn layer(w: f32, h: f32, radius: f32) -> El {
    el().absolute(0.0, 0.0).size(w, h).radius(radius).inert()
}

/// An asset's first still or its waveform, filling a `w`×`h` box.
pub(super) fn preview(
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

/// One file in the bin. A press selects it, a double-click adds it at the
/// playhead, a drag carries it to a lane and a right-click opens its menu.
pub(super) fn tile(ui: &Ui, asset: &BinAsset, w: f32, h: f32) -> El {
    let drag_path = asset.info.path.clone();
    let menu_path = drag_path.clone();
    picture(ui, asset, w, h)
        .help(hover(ui, asset))
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

/// The tile's layers, bottom to top: the card, its still or waveform, the
/// dim and spinner of a file that is not ready, then the outline.
fn picture(ui: &Ui, asset: &BinAsset, w: f32, h: f32) -> El {
    let c = c();
    let path = &asset.info.path;
    let ready = asset.state == AssetState::Ready;
    let clashes = ui.editor.clashes_with_format(&asset.info);
    let selected = ui.editor.selected_asset.as_deref() == Some(path.as_str());
    let thumb = ui.thumbs(path).first();
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
    tile.child(if selected {
        layer(w, h, theme::R_MD).border(2.0, c.brand)
    } else {
        layer(w, h, theme::R_MD)
            .border(1.0, Color::WHITE.alpha(70.0 / 255.0))
            .group_opacity(0.0, 1.0)
    })
}

/// The tile's tooltip: its name, metadata and path, then what it does.
fn hover(ui: &Ui, asset: &BinAsset) -> String {
    let meta = asset_meta(asset);
    let name = &asset.info.name;
    let path = &asset.info.path;
    let hover = if ui.editor.clashes_with_format(&asset.info) {
        format!(
            "{name}\n{meta}\n{path}\nIts orientation doesn't match the {} project, so it isn't added.",
            ui.editor.timeline.orientation.as_str()
        )
    } else if asset.state == AssetState::Ready {
        format!("{name}\n{meta}\n{path}\nDouble-click or drag onto the timeline to add.")
    } else {
        format!("{name}\n{meta}\nImporting…")
    };
    format!("{hover}\nRight-click to remove it.")
}

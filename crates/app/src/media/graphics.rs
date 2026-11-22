//! The dock's Graphics page: shapes and stickers to put on the timeline.
//! A press adds one at the playhead over the video there; a drag carries it
//! to a lane. Their pictures are drawn once per size and kept.

use std::cell::RefCell;
use std::collections::HashMap;

use eddie_core::editor::graphics::GRAPHIC_SECONDS;
use openreel_engine::{Graphic, Shape, ShapeKind};
use openreel_media::{STICKERS, draw_graphic, graphic_label};
use rune::{Color, Cursor, El, Phase, Pixels, ScrollSpec, SurfaceFit, col, el, row, text};
use rune_kit::{ScrollBar, ScrollBarStyle};

use super::{BinDrag, DRAG_START, GAP};
use crate::app::Ui;
use crate::theme::{self, c};
use crate::timeline;

/// The narrowest a tile gets before a row takes one fewer.
const MIN_TILE: f32 = 52.0;

/// How many square tiles fit a row `width` wide, and their side.
fn grid(width: f32) -> (usize, f32) {
    let columns = (((width + GAP) / (MIN_TILE + GAP)).floor() as usize).max(1);
    let side = ((width - GAP * (columns - 1) as f32) / columns as f32).floor();
    (columns, side)
}

/// A shape as its tile shows it: plain white.
pub(crate) fn plain(kind: ShapeKind) -> Graphic {
    Graphic::Shape(Shape::new(kind))
}

/// `graphic` drawn `long` pixels on its long side, drawn once and kept.
pub(crate) fn pixels(graphic: &Graphic, long: u32) -> Option<Pixels> {
    thread_local! {
        static DRAWN: RefCell<HashMap<(String, u32), Pixels>> = RefCell::new(HashMap::new());
    }
    let key = (format!("{graphic:?}"), long);
    DRAWN.with_borrow_mut(|drawn| {
        if let Some(pixels) = drawn.get(&key) {
            return Some(pixels.clone());
        }
        let frame = draw_graphic(graphic, long).ok()?;
        let pixels = Pixels::new();
        pixels.set(frame.width, frame.height, frame.rgba);
        drawn.insert(key, pixels.clone());
        Some(pixels)
    })
}

/// The page under the dock's tabs: Shapes, then Stickers.
pub(super) fn page(width: f32) -> El {
    let c = c();
    let (columns, side) = grid(width);
    let shapes = ShapeKind::ALL.map(plain);
    let stickers: Vec<Graphic> = STICKERS
        .iter()
        .map(|sticker| Graphic::Sticker {
            name: sticker.name.to_owned(),
        })
        .collect();
    col()
        .key("bin-graphics")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(
            col()
                .gap(10.0)
                .child(heading("Shapes"))
                .child(tiles("bin-shapes", &shapes, columns, side))
                .child(el().h(4.0))
                .child(heading("Stickers"))
                .child(tiles("bin-stickers", &stickers, columns, side)),
        )
        .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside())
}

fn heading(label: &str) -> El {
    text(label.to_owned())
        .font(theme::SMALL)
        .color(c().muted_fg)
        .single_line()
}

fn tiles(key: &str, graphics: &[Graphic], columns: usize, side: f32) -> El {
    let rows = graphics.chunks(columns).map(|graphics| {
        row().gap(GAP).children(
            graphics
                .iter()
                .map(|graphic| tile(graphic.clone(), side))
                .collect::<Vec<_>>(),
        )
    });
    col()
        .key(key.to_owned())
        .gap(GAP)
        .children(rows.collect::<Vec<_>>())
}

/// What a tile's key calls `graphic`: `shape-heart`, `sticker-fire`.
fn slug(graphic: &Graphic) -> String {
    match graphic {
        Graphic::Shape(shape) => format!("shape-{}", shape.kind.as_str()),
        Graphic::Sticker { name } => format!("sticker-{name}"),
    }
}

/// The picture of `graphic` in a `side` square, inset so it breathes.
pub(super) fn picture(graphic: &Graphic, side: f32) -> El {
    let inset = (side * 0.14).round();
    let inner = side - 2.0 * inset;
    let mut picture = el().absolute(inset, inset).size(inner, inner).inert();
    if let Some(pixels) = pixels(graphic, (inner * 2.0).round() as u32) {
        picture = picture.pixels_fit(&pixels, SurfaceFit::Fit);
    }
    picture
}

/// One shape or sticker. A press adds it at the playhead; a drag carries
/// it to a lane.
fn tile(graphic: Graphic, side: f32) -> El {
    let c = c();
    let name = graphic_label(&graphic);
    let layer = || {
        el().absolute(0.0, 0.0)
            .size(side, side)
            .radius(theme::R_MD)
            .inert()
    };
    let help =
        format!("{name}: add it at the playhead, over the video there, or drag it onto a lane");
    let dragged = graphic.clone();
    el().key(format!("bin-{}", slug(&graphic)))
        .size(side, side)
        .radius(theme::R_MD)
        .fill(c.card)
        .cursor(Cursor::Pointer)
        .hover_group()
        .role("button")
        .label(name)
        .help(help)
        .child(picture(&graphic, side))
        .child(
            layer()
                .border(1.0, Color::WHITE.alpha(70.0 / 255.0))
                .group_opacity(0.0, 1.0),
        )
        .on_drag(move |ui: &mut Ui, drag, _| match drag.phase {
            Phase::Down => {}
            Phase::Move => {
                if ui.bin_drag.is_some() || drag.dx.hypot(drag.dy) >= DRAG_START {
                    ui.bin_drag = Some(BinDrag {
                        path: String::new(),
                        graphic: Some(dragged.clone()),
                        at: drag.window_point(),
                        size: [side, side],
                    });
                }
            }
            Phase::Up => {
                let carried = ui.bin_drag.take().is_some();
                if drag.cancelled() {
                    return;
                }
                let (row, at) = if carried {
                    match timeline::drop_target(ui, drag.window_point()) {
                        Some((row, at)) => (Some(row), at),
                        None => return,
                    }
                } else {
                    (None, ui.editor.playhead)
                };
                let graphic = dragged.clone();
                if let Err(error) = ui.editor.add_graphic(graphic, row, at, GRAPHIC_SECONDS) {
                    ui.editor.status = error;
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use eddie_core::editor::media::{AssetState, BinAsset};
    use eddie_core::editor::sweep::SweepData;
    use openreel_engine::ClipKind;

    use super::*;
    use crate::testing::{self, FRAME, click, frames, shown, tooltip};

    #[test]
    fn the_default_panel_shows_four_square_tiles_a_row() {
        let width = super::super::DEFAULT_W - 2.0 * super::super::PAD_X;
        assert_eq!(grid(width), (4, 57.0));
        assert_eq!(grid(40.0), (1, 40.0));
    }

    #[test]
    fn a_shape_goes_over_the_video_at_the_playhead_and_a_sticker_where_it_is_dropped() {
        let dir = std::env::temp_dir().join(format!("eddie-bin-graphics-{}", std::process::id()));
        let mut ui = Ui::headless();
        ui.editor.graphics_dir = Some(dir.clone());
        ui.editor.assets.push(BinAsset {
            info: openreel_media::MediaInfo {
                path: "/media/a.mp4".to_owned(),
                name: "a.mp4".to_owned(),
                duration: 10.0,
                width: 320,
                height: 180,
                fps: 30.0,
                has_video: true,
                has_audio: false,
                photo: false,
                alpha: false,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: SweepData::default(),
        });
        let video = ui.editor.timeline.add_media_clip(
            "a.mp4",
            ClipKind::Video,
            0,
            0.0,
            10.0,
            0.0,
            "/media/a.mp4",
        );
        let mut rt = testing::window(ui);
        assert!(!shown(&rt, "bin-shapes"));
        click(&mut rt, "bin-tab-graphics");
        assert!(shown(&rt, "bin-shapes") && shown(&rt, "bin-sticker-thumbs-up"));
        assert!(
            !shown(&rt, "bin-import"),
            "Import belongs to the Media page"
        );

        // A press puts it at the playhead on a new lane over the video.
        click(&mut rt, "bin-shape-heart");
        let editor = &rt.state.editor;
        let heart = editor
            .selected
            .and_then(|id| editor.timeline.clip(id))
            .unwrap();
        assert_eq!(heart.graphic, Some(plain(ShapeKind::Heart)));
        assert_eq!(
            (heart.start, heart.duration, heart.track),
            (0.0, GRAPHIC_SECONDS, 0)
        );
        assert_eq!(editor.timeline.clip(video).unwrap().track, 1);
        assert!(rt.state.thumbs(heart.source.as_deref().unwrap()).len() == 1);

        // A drag lands where it is let go, on the lane under the pointer.
        let [left, top, width, height] = crate::timeline::lane_rect(&rt.state, 1).unwrap();
        let to = [left + width * 0.9, top + height / 2.0];
        let (row, at) = crate::timeline::drop_target(&rt.state, to).unwrap();
        assert!(at > GRAPHIC_SECONDS, "the drop is after the heart: {at}");
        let (x, y) = rt.find("bin-sticker-grinning-face").unwrap();
        testing::input_arrives();
        rt.pointer_move(x, y);
        rt.frame(FRAME);
        rt.pointer_down(x, y);
        rt.frame(FRAME);
        rt.pointer_move(x + 20.0, y + 20.0);
        rt.frame(FRAME);
        rt.pointer_move(to[0], to[1]);
        rt.frame(FRAME);
        assert!(shown(&rt, "bin-drag-ghost"));
        rt.pointer_up(to[0], to[1]);
        frames(&mut rt, 2);
        let editor = &rt.state.editor;
        let fire = editor
            .selected
            .and_then(|id| editor.timeline.clip(id))
            .unwrap();
        assert_eq!(fire.name, "Grinning face");
        assert_eq!(editor.timeline.row(ClipKind::Video, fire.track), row);
        assert!(
            (fire.start - at).abs() < 0.05,
            "{} against {at}",
            fire.start
        );

        for key in [
            "bin-tab-media",
            "bin-tab-graphics",
            "bin-shape-star",
            "bin-sticker-winking-face",
        ] {
            let help = tooltip(&mut rt, key);
            assert!(
                help.is_some_and(|help| !help.is_empty()),
                "{key} has no tooltip"
            );
        }
        click(&mut rt, "bin-tab-media");
        assert!(shown(&rt, "bin-search") && !shown(&rt, "bin-shapes"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

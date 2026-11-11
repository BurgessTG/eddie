//! The timeline dock: its toolbar, the ruler and the lanes.
//!
//! What a press, drag or wheel means is decided once, in `eddie_core`'s
//! `lanes`. This module draws the lanes and reports the pointer to it:
//! `clip` is one clip's element, `chrome` the lane headers, ruler and
//! playhead around the clips, and `toolbar` the bar above them.

mod chrome;
mod clip;
mod toolbar;

use std::time::{Duration, Instant};

use eddie_core::Editor;
use eddie_core::editor::lanes::{
    self, ClipHit, Gesture, HEADER_W, Point, Scene, TRACK_H, ViewRect,
};
use eddie_core::editor::media::BinAsset;
use openreel_engine::format_timecode;
use rune::{Color, Cursor, Cx, Drag, El, Phase, PointerEvent, Tx, WheelEvent, col, el, row, text};

use crate::app::Ui;
use crate::theme::{self, c};

/// Pointer travel that turns a press on the lanes into a drag.
const DRAG_START: f32 = 6.0;
/// How often the lanes scroll under a pointer held at their edge: once a
/// 60 Hz frame, which sets the scroll speed.
const AUTOSCROLL_EVERY: Duration = Duration::from_millis(16);
const HELP: &str = "Click to move the playhead. Drag clips to move, drag edges to trim. \
                    Ctrl+scroll zooms. F1 lists shortcuts.";

/// The pointer over the lanes, kept between frames.
#[derive(Default)]
pub struct LaneInput {
    gesture: Gesture,
    /// Where the held press landed, in the view's points.
    press: Option<Point>,
    /// The press has travelled far enough to be a drag.
    dragging: bool,
    /// The pointer over the view, for hover looks and the cursor.
    hover: Option<Point>,
    /// The held pointer in window points, where the drag readout sits.
    held_at: Option<Point>,
    /// When the lanes last scrolled under a pointer held at their edge.
    scrolled: Option<Instant>,
}

impl LaneInput {
    /// A press on the lanes is held, so the edit it makes is unfinished.
    pub fn held(&self) -> bool {
        self.press.is_some()
    }
}

fn white(alpha: u8) -> Color {
    Color::WHITE.alpha(f32::from(alpha) / 255.0)
}

fn black(alpha: u8) -> Color {
    theme::hex("#000000").alpha(f32::from(alpha) / 255.0)
}

/// The view in its own points, which the paint and the pointer share.
fn local(width: f32, height: f32) -> ViewRect {
    ViewRect {
        left: 0.0,
        top: 0.0,
        right: width,
        bottom: height,
    }
}

/// The view in window points.
fn window_rect(ui: &Ui) -> Option<ViewRect> {
    let [left, top, width, height] = ui.timeline_rect?;
    Some(ViewRect {
        left,
        top,
        right: left + width,
        bottom: top + height,
    })
}

/// The lane and time under a window point, for something dropped there.
/// `None` outside the lanes: over the headers, the ruler or another panel.
pub fn drop_target(ui: &Ui, at: [f32; 2]) -> Option<(usize, f64)> {
    lanes::drop_target(&ui.editor.viewport, window_rect(ui)?, at)
}

/// Lane `lane`'s rect right of the headers, `[x, y, w, h]` in window points.
pub fn lane_rect(ui: &Ui, lane: usize) -> Option<[f32; 4]> {
    let lane = lanes::lane_rect(window_rect(ui)?, lane);
    Some([lane.left, lane.top, lane.width(), lane.height()])
}

/// Once per editor frame: tell the editor how wide the lanes are, keep the
/// scroll inside the timeline and say whether a gesture is held.
pub fn sync(ui: &mut Ui) {
    ui.pointer_held = ui.lanes.held();
    ui.editor.viewport.interacting = ui.lanes.gesture.is_interacting();
    let Some([_, _, width, _]) = ui.timeline_rect else {
        return;
    };
    let lanes_w = (width - HEADER_W).max(0.0);
    ui.editor.viewport.width = Some(lanes_w);
    let end = ui.editor.timeline.end();
    lanes::clamp_scroll(&mut ui.editor.viewport, end, lanes_w);
}

/// What a gesture changes, and the bin it reads media lengths from.
fn scene(editor: &mut Editor, rect: ViewRect) -> (Scene<'_>, &[BinAsset]) {
    (
        Scene {
            timeline: &mut editor.timeline,
            playhead: &mut editor.playhead,
            selected: &mut editor.selected,
            port: &mut editor.viewport,
            rect,
        },
        &editor.assets,
    )
}

fn source_duration(assets: &[BinAsset], path: &str) -> Option<f64> {
    assets
        .iter()
        .find(|asset| asset.info.path == path)
        .map(|asset| asset.info.duration)
}

/// A press, its moves and its release over the view.
fn pointer(ui: &mut Ui, drag: &mut Drag, tx: &mut Tx) {
    let rect = local(drag.w, drag.h);
    let pos = [drag.x, drag.y];
    match drag.phase {
        Phase::Down => {
            // Deep in the header gutter nothing starts.
            ui.lanes.press = Gesture::accepts(rect, pos).then_some(pos);
            ui.lanes.dragging = false;
            ui.lanes.scrolled = None;
        }
        Phase::Move => {
            let Some(press) = ui.lanes.press else {
                return;
            };
            ui.lanes.hover = Some(pos);
            ui.lanes.held_at = Some(drag.window_point());
            let (mut scene, assets) = scene(&mut ui.editor, rect);
            let gesture = &mut ui.lanes.gesture;
            if ui.lanes.dragging {
                // Held near an edge the lanes keep scrolling, pointer
                // still or not, so the far end of a long clip is reachable.
                let mut scrolled = *scene.port;
                if gesture.autoscroll(&mut scrolled, rect, pos) {
                    let due = ui
                        .lanes
                        .scrolled
                        .is_none_or(|at| at.elapsed() >= AUTOSCROLL_EVERY);
                    if due {
                        *scene.port = scrolled;
                        ui.lanes.scrolled = Some(Instant::now());
                    }
                    tx.drag_repeat_after(AUTOSCROLL_EVERY);
                }
            } else {
                let travel = (pos[0] - press[0]).hypot(pos[1] - press[1]);
                if travel < DRAG_START {
                    return;
                }
                ui.lanes.dragging = true;
                gesture.begin(&mut scene, press, pos);
            }
            gesture.drag(&mut scene, pos, &|path| source_duration(assets, path));
        }
        Phase::Up => {
            let Some(press) = ui.lanes.press.take() else {
                return;
            };
            let dragging = std::mem::take(&mut ui.lanes.dragging);
            ui.lanes.held_at = None;
            ui.lanes.scrolled = None;
            let (mut scene, assets) = scene(&mut ui.editor, rect);
            let gesture = &mut ui.lanes.gesture;
            if !dragging {
                if !drag.cancelled() {
                    gesture.click(&mut scene, press, pos);
                }
            } else if drag.cancelled() {
                // A lost press puts a moved clip back.
                gesture.finish(&mut scene, None);
            } else {
                gesture.drag(&mut scene, pos, &|path| source_duration(assets, path));
                gesture.finish(&mut scene, Some(pos));
            }
        }
    }
    ui.editor.viewport.interacting = ui.lanes.gesture.is_interacting();
    ui.pointer_held = ui.lanes.held();
}

/// A wheel or two-finger scroll over the view: along time, or zoom about
/// the pointer with Ctrl.
fn wheel(ui: &mut Ui, wheel: WheelEvent, tx: &mut Tx) -> bool {
    let end = ui.editor.timeline.end();
    let port = &mut ui.editor.viewport;
    if wheel.mods.ctrl || wheel.mods.cmd {
        lanes::zoom_about(port, HEADER_W, wheel.x, (wheel.dy * 0.002).exp(), end);
    } else {
        lanes::scroll_by(port, wheel.dx, wheel.dy);
    }
    tx.instant();
    true
}

/// The ruler and lanes, and the layer over them that takes the pointer.
fn view(ui: &Ui) -> El {
    let c = c();
    let mut view = el()
        .key("timeline-view")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        // Lanes past the bottom are cut off; nothing here eases into place.
        .clip_soft(0.0)
        .instant_changes()
        .on_layout(|ui: &mut Ui, rect| ui.timeline_rect = Some(rect));
    let Some([_, _, width, height]) = ui.timeline_rect else {
        return view;
    };
    let rect = local(width, height);
    let lanes_w = (width - HEADER_W).max(0.0);
    let port = &ui.editor.viewport;
    let timeline = &ui.editor.timeline;
    let hovered = ui
        .lanes
        .hover
        .and_then(|at| lanes::hover_clip(port, timeline, rect, at))
        .map(|(clip, hit)| (clip.id, hit));

    let mut content = el()
        .key("timeline-lanes")
        .absolute(HEADER_W, 0.0)
        .size(lanes_w, height)
        .clip_soft(0.0)
        .inert();
    for (track, label) in timeline.track_labels.iter().enumerate() {
        let lane = lanes::lane_rect(rect, track);
        if lane.top > height {
            break;
        }
        view = view.child(chrome::header(timeline, track, label, lane.top));
        content = content.child(
            el().absolute(0.0, lane.top)
                .size(lanes_w, TRACK_H)
                .radius(theme::R_SM)
                .fill(c.background),
        );
    }
    content = chrome::ruler(port, lanes_w, content);
    for clip in &timeline.clips {
        let hover = hovered.and_then(|(id, hit)| (id == clip.id).then_some(hit));
        if let Some(clip) = clip::clip_el(ui, clip, rect, lanes_w, hover) {
            content = content.child(clip);
        }
    }
    let x = lanes::x_at(port, 0.0, ui.editor.playhead);
    if (0.0..=lanes_w).contains(&x) {
        content = content.child(chrome::playhead(x, height));
    }

    let mut input = el()
        .key("timeline-input")
        .absolute(0.0, 0.0)
        .size(width, height)
        .on_drag(pointer)
        .on_wheel(wheel)
        .on_pointer_move_local(|ui: &mut Ui, at: PointerEvent, _| {
            ui.lanes.hover = Some([at.x, at.y]);
        })
        .on_hover_change(|ui: &mut Ui, hovered, _| {
            if !hovered && !ui.lanes.held() {
                ui.lanes.hover = None;
            }
        });
    input = match hovered {
        Some((_, ClipHit::Body)) => input.cursor(Cursor::Grab),
        Some(_) => input.cursor(Cursor::ResizeColumn),
        None => input,
    };
    if !ui.lanes.held() {
        // No tooltip over a clip while it is dragged.
        input = input.help(HELP);
    }
    view.child(content).child(input)
}

/// What floats over the window for the lanes: where a dragged clip sits.
pub fn overlays(ui: &Ui, overlays: &mut Vec<El>) {
    let (Some(id), Some([x, y])) = (ui.lanes.gesture.dragged_clip(), ui.lanes.held_at) else {
        return;
    };
    let timeline = &ui.editor.timeline;
    let Some(clip) = timeline.clip(id) else {
        return;
    };
    let label = format!(
        "{} → {}  ·  src {}",
        format_timecode(clip.start, timeline.fps),
        format_timecode(clip.end(), timeline.fps),
        format_timecode(clip.source_in, timeline.fps),
    );
    overlays.push(
        row()
            .key("timeline-readout")
            .absolute(x + 14.0, y - 30.0)
            .size(label.len() as f32 * 6.6 + 16.0, 20.0)
            .pl(8.0)
            .radius(6.0)
            .fill(black(200))
            .center_y()
            .inert()
            .instant_changes()
            .child(
                text(label)
                    .font(theme::mono(10.5))
                    .color(c().foreground)
                    .single_line(),
            ),
    );
}

/// The bottom dock. It takes 27% of the window, between 170 and 240
/// points, and its least while the curator has the middle of the window.
pub fn panel(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    let c = c();
    let window_h = cx.layout_size("kit-root").map_or(1080.0, |[_, h]| h);
    let height = if ui.editor.curator.open {
        170.0
    } else {
        (window_h * 0.27).clamp(170.0, 240.0)
    };
    col()
        .key("timeline")
        .w_full()
        .h(height)
        .px(14.0)
        .py(10.0)
        .gap(14.0)
        .fill(c.surface)
        .border_sides([1.0, 0.0, 0.0, 0.0], c.border)
        .child(toolbar::toolbar(ui))
        .child(view(ui))
}

#[cfg(test)]
mod tests;

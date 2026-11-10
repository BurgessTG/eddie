//! The timeline dock: its toolbar, the ruler and the lanes.
//!
//! What a press, drag or wheel means is decided once, in `eddie_core`'s
//! `lanes`. This file draws the lanes and reports the pointer to it.

use std::time::{Duration, Instant};

use eddie_core::editor::lanes::{
    self, ClipHit, Gesture, HEADER_W, Point, RULER_H, Scene, TRACK_H, ViewRect,
};
use eddie_core::editor::media::BinAsset;
use eddie_core::editor::viewport::TimelineViewport;
use eddie_core::shortcuts;
use eddie_core::{Action, Editor};
use openreel_engine::{Clip, ClipKind, Timeline, format_timecode};
use rune::{
    Color, Cursor, Cx, Drag, El, LineCap, Phase, PointerEvent, Shape, Tx, WheelEvent, col, el, row,
    spacer, text,
};

use crate::app::{Thumb, Ui};
use crate::icons::{self, Tool};
use crate::theme::{self, c};

/// Pointer travel that turns a press on the lanes into a drag.
const DRAG_START: f32 = 6.0;
/// How often the lanes scroll under a pointer held at their edge: the
/// egui window's frame, so both scroll at the same speed.
const AUTOSCROLL_EVERY: Duration = Duration::from_millis(16);
/// How far a clip's element reaches past the lanes' edges. Only the part
/// in view is laid out, and its cut ends stay out of sight.
const OVERHANG: f32 = 8.0;

const ZOOM_MIN: f32 = 4.0;
const ZOOM_MAX: f32 = 600.0;
const SLIDER_W: f32 = 110.0;
const SLIDER_HANDLE: f32 = 24.0;

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
    let port = &mut ui.editor.viewport;
    if wheel.mods.ctrl || wheel.mods.cmd {
        lanes::zoom_about(port, HEADER_W, wheel.x, (wheel.dy * 0.002).exp());
    } else {
        lanes::scroll_by(port, wheel.dx, wheel.dy);
    }
    tx.instant();
    true
}

fn kind_index(kind: ClipKind) -> usize {
    match kind {
        ClipKind::Video => 0,
        ClipKind::Audio => 1,
        ClipKind::Title => 2,
    }
}

fn kind_color(kind: ClipKind) -> Color {
    let c = c();
    match kind {
        ClipKind::Video => c.clip_video,
        ClipKind::Audio => c.clip_audio,
        ClipKind::Title => c.clip_title,
    }
}

/// The kind colour × 1.3, under the pointer.
fn kind_hover(kind: ClipKind) -> Color {
    theme::hex(match kind {
        ClipKind::Video => "#3C608B",
        ClipKind::Audio => "#396D4E",
        ClipKind::Title => "#8B6C1F",
    })
}

/// The kind colour × 1.8, for a lane header's accent bar.
fn kind_accent(kind: ClipKind) -> Color {
    theme::hex(match kind {
        ClipKind::Video => "#5385C1",
        ClipKind::Audio => "#4F976C",
        ClipKind::Title => "#C1952B",
    })
}

/// A lane header's glyph in a 16×16 box: film frame, level bars, a T, or
/// an empty dot.
fn kind_glyph(kind: Option<ClipKind>, fg: Color) -> El {
    let shape = Shape::new().view_box(16.0, 16.0);
    let shape =
        match kind {
            Some(ClipKind::Video) => icons::outline(shape, 1.5, 3.0, 13.0, 10.0, 2.0, 1.4, fg)
                .fill(vec![[6.5, 5.5], [10.5, 8.0], [6.5, 10.5]], fg),
            Some(ClipKind::Audio) => [4.0_f32, 9.0, 12.0, 7.0, 3.0].into_iter().enumerate().fold(
                shape,
                |shape, (i, h)| {
                    let x = 2.0 + i as f32 * 3.0;
                    shape.line(
                        [x, 8.0 - h / 2.0],
                        [x, 8.0 + h / 2.0],
                        1.4,
                        LineCap::Butt,
                        fg,
                    )
                },
            ),
            Some(ClipKind::Title) => {
                return row().size(16.0, 16.0).center().child(
                    text("T".to_owned())
                        .font(theme::sized(13.0))
                        .color(fg)
                        .single_line(),
                );
            }
            None => icons::ring(shape, [8.0, 8.0], 3.5, 1.4, fg),
        };
    el().size(16.0, 16.0).shape(shape)
}

/// Two interlocked rings marking a linked picture and sound pair, 16×16.
fn link_glyph() -> Shape {
    [-2.5_f32, 2.5]
        .into_iter()
        .fold(Shape::new().view_box(16.0, 16.0), |shape, dx| {
            icons::outline(shape, 7.5 + dx, 5.5, 7.0, 5.0, 2.0, 1.3, white(200))
        })
}

/// A lane's header card. Lanes take any kind, so it shows the kind that
/// covers the most time there and stays neutral when the lane is empty.
fn header(timeline: &Timeline, track: usize, label: &str, top: f32) -> El {
    let c = c();
    let mut cover = [0.0_f64; 3];
    for clip in timeline.clips.iter().filter(|clip| clip.track == track) {
        cover[kind_index(clip.kind)] += clip.duration;
    }
    let kind = cover.iter().any(|time| *time > 0.0).then(|| {
        [ClipKind::Video, ClipKind::Audio, ClipKind::Title]
            .into_iter()
            .max_by(|a, b| cover[kind_index(*a)].total_cmp(&cover[kind_index(*b)]))
            .unwrap_or(ClipKind::Video)
    });
    let (accent, glyph_fg, label_fg) = match kind {
        Some(kind) => (kind_accent(kind), c.muted_fg, c.foreground),
        None => (c.border_strong, c.subtle_fg, c.subtle_fg),
    };
    let width = HEADER_W - 8.0;
    el().key(format!("lane-header-{track}"))
        .absolute(0.0, top)
        .size(width, TRACK_H)
        .radius(theme::R_SM)
        .fill(c.card)
        .inert()
        .child(
            el().absolute(0.0, 10.0)
                .size(3.0, TRACK_H - 20.0)
                .radius(2.0)
                .fill(accent),
        )
        .child(kind_glyph(kind, glyph_fg).absolute(14.0, (TRACK_H - 16.0) / 2.0))
        .child(
            row().absolute(38.0, 0.0).h(TRACK_H).center_y().child(
                text(label.to_owned())
                    .font(theme::sized(12.0))
                    .color(label_fg)
                    .single_line()
                    .truncate()
                    .max_w(width - 38.0 - 6.0),
            ),
        )
}

/// The ruler's ticks and labels, `width` wide from the lanes' left edge.
fn ruler(port: &TimelineViewport, width: f32, mut content: El) -> El {
    let c = c();
    let step = lanes::ruler_step(port.px_per_sec);
    let first = (lanes::time_at(port, 0.0, 0.0) / step).floor() * step;
    let minor = step / 4.0;
    // One point taller than the ruler, for the baseline under it.
    let mut ticks = Shape::new().view_box(width, RULER_H + 1.0);
    let mut labels = Vec::new();
    for i in 0_u32.. {
        let t = first + f64::from(i) * minor;
        // On the pixel grid, so each tick is one sharp column.
        let x = lanes::x_at(port, 0.0, t).floor() + 0.5;
        if x > width {
            break;
        }
        let major = i % 4 == 0;
        ticks = ticks.line(
            [x, RULER_H - if major { 9.0 } else { 4.0 }],
            [x, RULER_H],
            1.0,
            LineCap::Butt,
            if major { c.border_strong } else { c.border },
        );
        if major && t >= 0.0 {
            labels.push(
                text(lanes::ruler_label(t, step))
                    .font(theme::sized(10.5))
                    .color(c.subtle_fg)
                    .single_line()
                    .absolute(x + 5.5, 3.0),
            );
        }
    }
    ticks = ticks.line(
        [0.0, RULER_H + 0.5],
        [width, RULER_H + 0.5],
        1.0,
        LineCap::Butt,
        c.border,
    );
    content = content.child(
        el().absolute(0.0, 0.0)
            .size(width, RULER_H + 1.0)
            .shape(ticks),
    );
    labels
        .into_iter()
        .fold(content, |content, label| content.child(label))
}

/// A clip's pictures: stills chosen by the source time under each tile,
/// so the strip follows trims as the waveform does.
fn filmstrip(
    mut body: El,
    mut art: Shape,
    clip: &Clip,
    thumbs: &[Thumb],
    px_per_sec: f32,
    left: f32,
    (w, h): (f32, f32),
) -> (El, Shape) {
    let Some(first) = thumbs.first() else {
        return (body, art);
    };
    let aspect = if first.height > 0 {
        first.width as f32 / first.height as f32
    } else {
        16.0 / 9.0
    };
    let tile_w = (h * aspect).max(12.0);
    let opacity = clip.opacity.clamp(0.0, 1.0) * 230.0 / 255.0;
    for tile in lanes::filmstrip_tiles(clip.source_in, clip.duration, px_per_sec, tile_w) {
        let x = left + tile.x;
        if x + tile.w < 0.0 || x > w {
            continue;
        }
        let index = thumbs
            .partition_point(|thumb| thumb.time <= tile.center_source)
            .saturating_sub(1);
        // A tile cut by the clip's edge hangs over it; the body clips it.
        body = body.child(
            el().absolute(x - tile.uv.0 * tile_w, 0.0)
                .size(tile_w, h)
                .pixels(&thumbs[index].pixels)
                .opacity(opacity),
        );
        // Separators, so the stills read as discrete frames.
        if tile.x > 0.5 {
            art = art.line([x, 0.0], [x, h], 1.0, LineCap::Butt, black(110));
        }
    }
    (body, art)
}

/// A clip's peaks, read through its source range so trims and splits show
/// the right part of the sound.
fn waveform(
    mut art: Shape,
    clip: &Clip,
    asset: &BinAsset,
    px_per_sec: f32,
    left: f32,
    full_w: f32,
    (w, h): (f32, f32),
) -> Shape {
    if asset.peaks.is_empty() || asset.info.duration <= 0.0 {
        return art;
    }
    let peaks_per_sec = asset.peaks.len() as f64 / asset.info.duration;
    // Below the name chip, so the label never sits on the peaks.
    let mid = h / 2.0 + 5.0;
    let max_amp = h / 2.0 - 8.0;
    // CLIP_AUDIO × 2.6.
    let color = theme::hex("#72DA9C");
    // Bars sit every 3 points from the clip's own edge; start at the first
    // one in view.
    let mut x = 2.0 + ((-left - 2.0) / 3.0).floor().max(0.0) * 3.0;
    while x < full_w - 2.0 && left + x <= w {
        let source_time = clip.source_in + f64::from(x) / f64::from(px_per_sec);
        if let Some(peak) = asset.peaks.get((source_time * peaks_per_sec) as usize) {
            // Square-root scale, so quiet dialogue stays visible beside
            // loud music.
            let amp = (peak.clamp(0.0, 1.0).sqrt() * max_amp).max(1.0);
            art = art.line(
                [left + x, mid - amp],
                [left + x, mid + amp],
                2.0,
                LineCap::Butt,
                color,
            );
        }
        x += 3.0;
    }
    art
}

/// One clip in the lanes' points, or `None` when it is out of view.
fn clip_el(
    ui: &Ui,
    clip: &Clip,
    view: ViewRect,
    lanes_w: f32,
    hover: Option<ClipHit>,
) -> Option<El> {
    let c = c();
    let port = &ui.editor.viewport;
    let rect = lanes::clip_rect(port, view, clip);
    let (clip_left, clip_right) = (rect.left - HEADER_W, rect.right - HEADER_W);
    if clip_right < 0.0 || clip_left > lanes_w {
        return None;
    }
    let from = clip_left.max(-OVERHANG);
    let to = clip_right.min(lanes_w + OVERHANG);
    let (w, h) = (to - from, rect.height());
    // The clip's own left edge and width, in the element's points.
    let (left, full_w) = (clip_left - from, clip_right - clip_left);

    let selected = ui.editor.selected == Some(clip.id);
    let fill = if hover.is_some() && !selected {
        kind_hover(clip.kind)
    } else {
        kind_color(clip.kind)
    };
    let mut body = el()
        .key(format!("clip-{}", clip.id))
        .absolute(from, rect.top)
        .size(w, h)
        .radius(theme::R_SM)
        .fill(fill)
        .clip_rounded(theme::R_SM);

    let mut art = Shape::new().view_box(w, h);
    let source = clip.source.as_deref();
    match clip.kind {
        ClipKind::Video => {
            let thumbs = source.map_or(&[][..], |path| ui.thumbs(path));
            (body, art) = filmstrip(body, art, clip, thumbs, port.px_per_sec, left, (w, h));
        }
        ClipKind::Audio => {
            let asset = source.and_then(|path| {
                ui.editor
                    .assets
                    .iter()
                    .find(|asset| asset.info.path == path)
            });
            if let Some(asset) = asset {
                art = waveform(art, clip, asset, port.px_per_sec, left, full_w, (w, h));
            }
        }
        ClipKind::Title => {}
    }
    // A lighter top edge gives the body depth without an outline.
    let (top_from, top_to) = ((left + 4.0).max(0.0), (left + full_w - 4.0).min(w));
    if top_to > top_from {
        art = art.line(
            [top_from, 0.5],
            [top_to, 0.5],
            1.0,
            LineCap::Butt,
            white(40),
        );
    }
    // Trim affordance: a grip on the edge under the pointer.
    if let Some(hit @ (ClipHit::Left | ClipHit::Right)) = hover {
        let x = left
            + if hit == ClipHit::Left {
                3.5
            } else {
                full_w - 3.5
            };
        art = art.line(
            [x, h / 2.0 - 8.0],
            [x, h / 2.0 + 8.0],
            2.5,
            LineCap::Butt,
            c.foreground,
        );
    }
    body = body.child(el().absolute(0.0, 0.0).size(w, h).shape(art));

    if full_w > 40.0 {
        let linked = clip.linked_clip.is_some();
        let link_w = if linked { 16.0 } else { 0.0 };
        let mut chip = row()
            .absolute(left + 3.0, 3.0)
            .h(16.0)
            .px(5.0)
            .radius(4.0)
            .center_y()
            .child(
                text(clip.name.clone())
                    .font(theme::sized(11.0))
                    .color(white(235))
                    .single_line()
                    .truncate()
                    .max_w((full_w - 14.0 - link_w).max(8.0)),
            );
        // Media carries pictures or peaks under its name, so it gets a
        // dark chip; flat title bodies read without one.
        if clip.kind != ClipKind::Title {
            chip = chip.fill(black(140));
        }
        if linked {
            chip = chip.child(el().size(16.0, 16.0).shape(link_glyph()));
        }
        body = body.child(chip);
    }

    let outline = if selected {
        Some((2.0, c.brand))
    } else {
        hover.map(|_| (1.0, white(90)))
    };
    if let Some((width, color)) = outline {
        body = body.child(
            el().absolute(0.0, 0.0)
                .size(w, h)
                .radius(theme::R_SM)
                .border(width, color),
        );
    }
    Some(body)
}

/// The playhead: a line down the view with a pentagon head on the ruler.
fn playhead(x: f32, height: f32) -> El {
    let color = c().destructive;
    let top = RULER_H - 15.0;
    let shape = Shape::new()
        .view_box(11.0, height)
        .line([5.5, 0.0], [5.5, height], 1.5, LineCap::Butt, color)
        .fill(
            vec![
                [0.0, top],
                [11.0, top],
                [11.0, top + 8.0],
                [5.5, top + 14.0],
                [0.0, top + 8.0],
            ],
            color,
        );
    el().key("playhead")
        .absolute(x - 5.5, 0.0)
        .size(11.0, height)
        .shape(shape)
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
        view = view.child(header(timeline, track, label, lane.top));
        content = content.child(
            el().absolute(0.0, lane.top)
                .size(lanes_w, TRACK_H)
                .radius(theme::R_SM)
                .fill(c.background),
        );
    }
    content = ruler(port, lanes_w, content);
    for clip in &timeline.clips {
        let hover = hovered.and_then(|(id, hit)| (id == clip.id).then_some(hit));
        if let Some(clip) = clip_el(ui, clip, rect, lanes_w, hover) {
            content = content.child(clip);
        }
    }
    let x = lanes::x_at(port, 0.0, ui.editor.playhead);
    if (0.0..=lanes_w).contains(&x) {
        content = content.child(playhead(x, height));
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

/// Where the zoom handle's centre sits along the slider: logarithmic, so
/// each step of the way zooms by the same factor.
fn zoom_fraction(px_per_sec: f32) -> f32 {
    ((px_per_sec / ZOOM_MIN).ln() / (ZOOM_MAX / ZOOM_MIN).ln()).clamp(0.0, 1.0)
}

fn zoom_at(fraction: f32) -> f32 {
    ZOOM_MIN * (ZOOM_MAX / ZOOM_MIN).powf(fraction.clamp(0.0, 1.0))
}

fn zoom_slider(ui: &Ui) -> El {
    let c = c();
    let zoom = ui.editor.viewport.px_per_sec;
    let travel = SLIDER_W - SLIDER_HANDLE;
    let x = SLIDER_HANDLE / 2.0 + zoom_fraction(zoom) * travel;
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
        .range_value(f64::from(zoom), f64::from(ZOOM_MIN), f64::from(ZOOM_MAX))
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
            let port = &mut ui.editor.viewport;
            port.zoom(zoom_at(fraction) / port.px_per_sec, ui.editor.playhead);
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

fn toolbar(ui: &Ui) -> El {
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
        .child(toolbar(ui))
        .child(view(ui))
}

#[cfg(test)]
mod tests {
    use rune::{Env, Mods, Runtime};

    use super::*;
    use crate::app;

    const FRAME: f64 = 1.0 / 60.0;

    /// A window around an empty editor with one title clip from 2 s to 5 s
    /// on the first lane.
    fn window() -> (Runtime<Ui>, u64) {
        let mut ui = Ui::headless();
        let id = ui
            .editor
            .timeline
            .add_clip("Title", ClipKind::Title, 0, 2.0, 3.0, 0.0);
        let mut rt = Runtime::new(ui, app::view, (1280.0, 800.0), Env::new(true, 1.0));
        for _ in 0..3 {
            rt.frame(FRAME);
        }
        (rt, id)
    }

    /// The window point at `time` on lane `track`.
    fn at(rt: &Runtime<Ui>, time: f64, track: usize) -> (f32, f32) {
        let rect = window_rect(&rt.state).expect("the lanes are laid out");
        let lane = lanes::lane_rect(rect, track);
        (
            lanes::x_at(&rt.state.editor.viewport, rect.content_left(), time),
            lane.top + TRACK_H / 2.0,
        )
    }

    fn press_drag_release(rt: &mut Runtime<Ui>, from: (f32, f32), to: (f32, f32)) {
        rt.pointer_move(from.0, from.1);
        rt.frame(FRAME);
        rt.pointer_down(from.0, from.1);
        rt.frame(FRAME);
        for step in 1..=4 {
            let t = step as f32 / 4.0;
            rt.pointer_move(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            rt.frame(FRAME);
        }
        rt.pointer_up(to.0, to.1);
        rt.frame(FRAME);
        rt.frame(FRAME);
    }

    #[test]
    fn a_click_on_a_clip_selects_it_and_parks_the_playhead() {
        let (mut rt, id) = window();
        let point = at(&rt, 3.0, 0);
        press_drag_release(&mut rt, point, point);
        assert_eq!(rt.state.editor.selected, Some(id));
        assert!((rt.state.editor.playhead - 3.0).abs() < 0.05);
        assert!(!rt.state.pointer_held);

        // A click on an empty part of the lane clears the selection.
        let empty = at(&rt, 8.0, 0);
        press_drag_release(&mut rt, empty, empty);
        assert_eq!(rt.state.editor.selected, None);
        assert!((rt.state.editor.playhead - 8.0).abs() < 0.05);
    }

    #[test]
    fn dragging_a_clip_body_moves_it() {
        let (mut rt, id) = window();
        rt.state.editor.viewport.magnet = false;
        let from = at(&rt, 3.5, 0);
        // 96 points right at 48 points a second: two seconds later.
        press_drag_release(&mut rt, from, (from.0 + 96.0, from.1));
        let clip = rt.state.editor.timeline.clip(id).expect("the clip");
        assert!((clip.start - 4.0).abs() < 0.05, "start {}", clip.start);
        assert!((clip.duration - 3.0).abs() < 1e-6);
        assert!(!rt.state.editor.viewport.interacting);
    }

    #[test]
    fn dragging_a_clip_edge_trims_it() {
        let (mut rt, id) = window();
        rt.state.editor.viewport.magnet = false;
        let edge = at(&rt, 5.0, 0);
        // Just inside the right edge, one second to the left.
        press_drag_release(&mut rt, (edge.0 - 3.0, edge.1), (edge.0 - 51.0, edge.1));
        let clip = rt.state.editor.timeline.clip(id).expect("the clip");
        assert!((clip.start - 2.0).abs() < 1e-6, "start {}", clip.start);
        assert!(
            (clip.duration - 2.0).abs() < 0.05,
            "duration {}",
            clip.duration
        );
    }

    #[test]
    fn a_short_wobble_is_a_click_not_a_move() {
        let (mut rt, id) = window();
        let from = at(&rt, 3.5, 0);
        press_drag_release(&mut rt, from, (from.0 + 3.0, from.1));
        let clip = rt.state.editor.timeline.clip(id).expect("the clip");
        assert_eq!(clip.start, 2.0);
        assert_eq!(rt.state.editor.selected, Some(id));
    }

    #[test]
    fn dragging_on_the_ruler_scrubs_the_playhead() {
        let (mut rt, _) = window();
        let rect = window_rect(&rt.state).expect("the lanes are laid out");
        let from = at(&rt, 1.0, 0);
        let y = rect.top + RULER_H / 2.0;
        press_drag_release(&mut rt, (from.0, y), (from.0 + 144.0, y));
        assert!((rt.state.editor.playhead - 4.0).abs() < 0.05);
    }

    #[test]
    fn the_wheel_scrolls_and_ctrl_wheel_zooms_about_the_pointer() {
        let (mut rt, _) = window();
        // Something long enough to scroll through.
        rt.state
            .editor
            .timeline
            .add_clip("Long", ClipKind::Title, 0, 10.0, 120.0, 0.0);
        rt.frame(FRAME);
        let (x, y) = at(&rt, 4.0, 0);
        rt.pointer_move(x, y);
        rt.wheel_event(WheelEvent::new(0.0, -120.0).at(x, y));
        rt.frame(FRAME);
        rt.frame(FRAME);
        assert_eq!(rt.state.editor.viewport.scroll_x, 120.0);

        let (x, y) = at(&rt, 6.0, 0);
        let ctrl = Mods {
            ctrl: true,
            cmd: true,
            ..Mods::default()
        };
        rt.pointer_move(x, y);
        rt.wheel_event(WheelEvent::new(0.0, 200.0).at(x, y).mods(ctrl));
        rt.frame(FRAME);
        rt.frame(FRAME);
        let port = rt.state.editor.viewport;
        assert!(port.px_per_sec > 70.0, "zoom {}", port.px_per_sec);
        // The time under the pointer stayed under it.
        let rect = window_rect(&rt.state).expect("the lanes are laid out");
        let now = lanes::time_at(&port, rect.content_left(), x);
        assert!((now - 6.0).abs() < 0.02, "time under the pointer {now}");
    }

    /// Trimming a clip's left edge away from its neighbour leaves a gap.
    /// The magnet only snaps edges while dragging: the frames after the
    /// release must not ripple the gap closed.
    #[test]
    fn a_left_trim_with_the_magnet_on_survives_the_frames_after_it() {
        let mut ui = Ui::headless();
        assert!(ui.editor.viewport.magnet);
        let a = ui.editor.timeline.add_media_clip(
            "a.mp4",
            ClipKind::Video,
            0,
            0.0,
            10.0,
            0.0,
            "/media/a.mp4",
        );
        let b = ui.editor.timeline.add_media_clip(
            "b.mp4",
            ClipKind::Video,
            0,
            10.0,
            10.0,
            0.0,
            "/media/b.mp4",
        );
        // The whole window, editor pump included.
        let mut rt = crate::testing::window(ui);
        let edge = at(&rt, 10.0, 0);
        // Just inside B's left edge, three seconds to the right.
        press_drag_release(&mut rt, (edge.0 + 3.0, edge.1), (edge.0 + 147.0, edge.1));
        for _ in 0..10 {
            rt.frame(FRAME);
        }
        let timeline = &rt.state.editor.timeline;
        let (a, b) = (timeline.clip(a).unwrap(), timeline.clip(b).unwrap());
        assert_eq!((a.start, a.end()), (0.0, 10.0));
        assert!((b.start - 13.0).abs() < 0.05, "start {}", b.start);
        assert!((b.end() - 20.0).abs() < 1e-6, "end {}", b.end());
        assert!(
            (b.source_in - 3.0).abs() < 0.05,
            "source in {}",
            b.source_in
        );
    }

    #[test]
    fn the_zoom_slider_is_logarithmic_and_drags() {
        // Half way along is the geometric middle of the range.
        assert!((zoom_at(0.5) - (ZOOM_MIN * ZOOM_MAX).sqrt()).abs() < 0.01);
        assert!((zoom_fraction(zoom_at(0.3)) - 0.3).abs() < 1e-4);

        let (mut rt, _) = window();
        let (x, y) = rt.find("timeline-zoom").expect("the zoom slider");
        let right = x + SLIDER_W / 2.0;
        press_drag_release(&mut rt, (x, y), (right + 20.0, y));
        assert_eq!(rt.state.editor.viewport.px_per_sec, ZOOM_MAX);
    }
}

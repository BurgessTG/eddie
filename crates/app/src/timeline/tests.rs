use eddie_core::editor::lanes::RULER_H;
use eddie_core::editor::viewport::{MAX_ZOOM, MIN_ZOOM};
use openreel_engine::ClipKind;
use rune::{Env, Mods, Runtime};

use super::toolbar::{SLIDER_W, zoom_at, zoom_fraction};
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

/// The window point at `time` on `row`, counted from the top: the first
/// title lane is row 0 and the first video lane row 1.
fn at(rt: &Runtime<Ui>, time: f64, row: usize) -> (f32, f32) {
    let rect = window_rect(&rt.state).expect("the lanes are laid out");
    let lane = lanes::lane_rect(rect, row);
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
    let edge = at(&rt, 10.0, 1);
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
    assert!((zoom_at(0.5, MIN_ZOOM) - (MIN_ZOOM * MAX_ZOOM).sqrt()).abs() < 0.01);
    assert!((zoom_fraction(zoom_at(0.3, 0.5), 0.5) - 0.3).abs() < 1e-4);

    let (mut rt, _) = window();
    let (x, y) = rt.find("timeline-zoom").expect("the zoom slider");
    let (left, right) = (x - SLIDER_W / 2.0, x + SLIDER_W / 2.0);
    press_drag_release(&mut rt, (x, y), (right + 20.0, y));
    assert_eq!(rt.state.editor.viewport.px_per_sec, MAX_ZOOM);
    press_drag_release(&mut rt, (right - 4.0, y), (left - 20.0, y));
    assert_eq!(rt.state.editor.viewport.px_per_sec, MIN_ZOOM);

    // The left end shows all of a timeline too long for the usual floor.
    // The editor pump tells the viewport how wide the lanes are.
    let mut ui = Ui::headless();
    ui.editor
        .timeline
        .add_clip("Long", ClipKind::Title, 0, 0.0, 3600.0, 0.0);
    let mut rt = crate::testing::window(ui);
    let (x, y) = rt.find("timeline-zoom").expect("the zoom slider");
    let left = x - SLIDER_W / 2.0;
    press_drag_release(&mut rt, (left + 4.0, y), (left - 20.0, y));
    let port = rt.state.editor.viewport;
    let width = port.width.expect("the lanes are laid out");
    let span = 3600.0 * port.px_per_sec;
    assert!(span < width && span > width - 30.0, "{span} of {width}");
}

/// With the magnet on, a clip let go a few points short of its neighbour
/// meets it exactly and the two keep their order; with it off the clip
/// stays where it was let go.
#[test]
fn the_magnet_butts_clips_together_only_while_it_is_on() {
    for magnet in [true, false] {
        let (mut rt, title) = window();
        let timeline = &mut rt.state.editor.timeline;
        timeline.remove_clip(title);
        let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 0, 2.0, 1.2345, 0.0, "/a.mp4");
        let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 0, 6.0, 2.0, 0.0, "/b.mp4");
        rt.state.editor.viewport.magnet = magnet;
        rt.frame(FRAME);
        let from = at(&rt, 2.6, 1);
        // Its end let go five points (about 0.1 s) short of the neighbour.
        let to = at(&rt, 2.6 + (6.0 - 3.2345) - 5.0 / 48.0, 1);
        press_drag_release(&mut rt, from, to);
        let timeline = &rt.state.editor.timeline;
        let (a, b) = (timeline.clip(a).unwrap(), timeline.clip(b).unwrap());
        assert_eq!(b.start, 6.0, "magnet={magnet}: the neighbour stays");
        let gap = b.start - a.end();
        if magnet {
            assert!(gap.abs() < 1e-9, "flush: {gap}");
        } else {
            assert!(gap > 0.08, "free: {gap}");
        }
    }
}

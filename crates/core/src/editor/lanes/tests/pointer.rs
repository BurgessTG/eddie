//! Clicks, the ruler, the wheel and drops from outside.

use super::*;

#[test]
fn snaps_to_nearest_candidate_within_threshold() {
    let candidates = [0.0, 2.0, 5.0];
    assert_eq!(snap_to(1.9, &candidates, 0.25), 2.0);
    assert_eq!(snap_to(5.2, &candidates, 0.25), 5.0);
}

#[test]
fn filmstrip_tiles_anchor_to_source_time_not_clip_edge() {
    // 48px/s, 72px tiles -> one fragment per 1.5s of source.
    let tiles = filmstrip_tiles(3.5, 6.0, 48.0, 72.0);
    // First fragment covers source 3.0..4.5: only its right part
    // is inside the clip, UV-cropped instead of re-phased.
    let first = tiles[0];
    assert_eq!(first.x, 0.0);
    assert!((first.w - 48.0).abs() < 0.01, "w {}", first.w);
    assert!((first.uv.0 - 1.0 / 3.0).abs() < 0.01, "uv0 {}", first.uv.0);
    assert!((first.uv.1 - 1.0).abs() < 0.01);
    assert!((first.center_source - 3.75).abs() < 1e-9);
    // Fragments are equally spaced and cover the clip width.
    for pair in tiles.windows(2) {
        assert!((pair[1].x - (pair[0].x + pair[0].w)).abs() < 0.01);
    }
    let last = tiles[tiles.len() - 1];
    assert!((last.x + last.w - 288.0).abs() < 0.01);

    // Trimming the head by 0.5s must not re-phase later
    // fragments: the fragment covering source 6.75 keeps its
    // center and UV span, only its clip-local x shifts.
    let trimmed = filmstrip_tiles(4.0, 5.5, 48.0, 72.0);
    let before = tiles
        .iter()
        .find(|t| (t.center_source - 6.75).abs() < 1e-9)
        .expect("fragment at 6.75 before trim");
    let after = trimmed
        .iter()
        .find(|t| (t.center_source - 6.75).abs() < 1e-9)
        .expect("fragment at 6.75 after trim");
    assert_eq!(before.uv, after.uv);
    assert!((before.x - after.x - 24.0).abs() < 0.01);
}

#[test]
fn does_not_snap_beyond_threshold() {
    let candidates = [0.0, 2.0, 5.0];
    assert_eq!(snap_to(3.0, &candidates, 0.25), 3.0);
}

#[test]
fn a_click_parks_the_playhead_and_selects_what_is_under_it() {
    let mut timeline = Timeline::new(30.0);
    let id = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 3.0, 0.0, "a");
    let mut stage = Stage::new(timeline);
    let on_clip = stage.point(3.0, 0);
    let (mut scene, gesture) = stage.scene();
    gesture.click(&mut scene, on_clip, on_clip);
    assert_eq!(stage.selected, Some(id));
    assert_eq!(stage.playhead, 3.0);
    assert_eq!(stage.timeline.clip(id).unwrap().start, 2.0);

    let empty = stage.point(9.0, 0);
    let (mut scene, gesture) = stage.scene();
    gesture.click(&mut scene, empty, empty);
    assert_eq!(stage.selected, None);
    assert_eq!(stage.playhead, 9.0);
}

#[test]
fn a_ruler_drag_scrubs_and_leaves_clips_and_selection_alone() {
    let mut timeline = Timeline::new(30.0);
    let id = timeline.add_media_clip("a", ClipKind::Video, 0, 0.0, 3.0, 0.0, "a");
    let mut stage = Stage::new(timeline);
    let before = stage.timeline.clone();
    let x = |stage: &Stage, time| x_at(&stage.port, VIEW.content_left(), time);
    let (press, to) = ([x(&stage, 1.0), 8.0], [x(&stage, 2.5), 120.0]);
    let (mut scene, gesture) = stage.scene();
    gesture.begin(&mut scene, press, press);
    assert!(gesture.is_interacting());
    assert_eq!(gesture.dragged_clip(), None);
    gesture.drag(&mut scene, to, &|_| None);
    gesture.finish(&mut scene, Some(to));
    assert_eq!(stage.playhead, 2.5);
    assert_eq!(stage.selected, None);
    assert_eq!(stage.timeline, before);
    assert!(stage.timeline.clip(id).is_some());
}

#[test]
fn the_ruler_keeps_its_labels_apart_however_far_out() {
    // From the closest zoom to a 1,000,000 s timeline fitted in 40 px.
    for px_per_sec in [600.0, 48.0, 4.0, 0.25, 0.01, 0.000_04_f32] {
        let step = ruler_step(px_per_sec);
        let apart = step as f32 * px_per_sec;
        assert!(apart >= 72.0, "{px_per_sec} px/s: labels {apart} px apart");
    }
    assert_eq!(ruler_step(48.0), 2.0);
    assert_eq!(ruler_step(0.25), 300.0);
    assert_eq!(ruler_step(0.01), 7200.0);

    assert_eq!(ruler_label(0.5, 0.25), "0:00.5");
    assert_eq!(ruler_label(75.0, 5.0), "1:15");
    assert_eq!(ruler_label(3725.0, 5.0), "1:02:05");
    assert_eq!(ruler_label(7200.0, 3600.0), "2:00:00");
}

#[test]
fn wheel_scrolls_along_time_and_zoom_keeps_the_time_under_the_pointer() {
    let mut port = TimelineViewport::default();
    scroll_by(&mut port, 0.0, -120.0);
    assert_eq!(port.scroll_x, 120.0);
    scroll_by(&mut port, 500.0, 0.0);
    assert_eq!(port.scroll_x, 0.0);

    let content_left = VIEW.content_left();
    port.scroll_x = 96.0;
    let pointer_x = content_left + 240.0;
    let under = time_at(&port, content_left, pointer_x);
    zoom_about(&mut port, content_left, pointer_x, 2.0, 10.0);
    assert_eq!(port.px_per_sec, 96.0);
    assert!((time_at(&port, content_left, pointer_x) - under).abs() < 1e-4);

    // The end of a 10 s timeline can come back to mid-lanes, no further.
    port.scroll_x = 5000.0;
    clamp_scroll(&mut port, 10.0, 800.0);
    assert_eq!(port.scroll_x, 10.0 * 96.0 - 400.0);
}

#[test]
fn drops_from_outside_land_on_lanes_only() {
    let port = TimelineViewport::default();
    let lane_y = VIEW.lanes_top() + TRACK_H + TRACK_GAP + 5.0;
    assert_eq!(
        drop_target(&port, VIEW, [VIEW.content_left() + 96.0, lane_y]),
        Some((1, 2.0))
    );
    assert_eq!(drop_target(&port, VIEW, [20.0, lane_y]), None);
    assert_eq!(
        drop_target(&port, VIEW, [VIEW.content_left() + 96.0, 10.0]),
        None
    );
    assert_eq!(drop_target(&port, VIEW, [1300.0, lane_y]), None);
}

#[test]
fn a_click_on_a_trim_handle_selects_without_trimming_and_a_ruler_click_keeps_the_selection() {
    // The body, then three points inside each handle.
    for (x, time) in [(384.0, 5.0), (246.0, 2.125), (522.0, 7.875)] {
        let (mut stage, id) = clip_fixture();
        let before = stage.timeline.clone();
        stage.click([x, 99.0]);
        assert_eq!(stage.selected, Some(id));
        assert_eq!(stage.playhead, stage.timeline.snap(time));
        assert_eq!(stage.timeline, before, "x={x}");
        assert!(!stage.gesture.is_interacting());
    }

    let (mut stage, id) = clip_fixture();
    stage.selected = Some(id);
    stage.click([432.0, 10.0]);
    assert_eq!(stage.playhead, 6.0);
    assert_eq!(stage.selected, Some(id));
    assert!(!stage.gesture.is_interacting());
}

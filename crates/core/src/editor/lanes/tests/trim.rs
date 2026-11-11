//! Edge drags: which edge a press takes and where a trim stops.

use super::*;

#[test]
fn a_body_drag_moves_and_an_edge_drag_trims_from_where_the_press_began() {
    let mut timeline = Timeline::empty_project();
    let id = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 4.0, 1.0, "a");
    let mut stage = Stage::new(timeline);
    stage.port.magnet = false;

    // The press sits inside the right handle; the pointer has already
    // travelled into the body when the drag is recognised.
    let press = [stage.point(6.0, 0)[0] - 3.0, stage.point(6.0, 0)[1]];
    let to = [press[0] - 48.0, press[1]];
    stage.drag(press, to);
    let clip = stage.timeline.clip(id).unwrap();
    assert_eq!((clip.start, clip.duration, clip.source_in), (2.0, 3.0, 1.0));

    let from = stage.point(3.5, 0);
    stage.drag(from, [from[0] + 96.0, from[1]]);
    let clip = stage.timeline.clip(id).unwrap();
    assert_eq!((clip.start, clip.duration, clip.source_in), (4.0, 3.0, 1.0));
    assert_eq!(stage.selected, Some(id));
    assert_eq!(stage.playhead, 0.0, "a drag does not move the playhead");
}

#[test]
fn a_right_trim_stops_at_the_end_of_the_source() {
    let mut timeline = Timeline::empty_project();
    let id = timeline.add_media_clip("a", ClipKind::Video, 0, 0.0, 2.0, 1.0, "a.mp4");
    let mut stage = Stage::new(timeline);
    stage.port.magnet = false;
    let press = [stage.point(2.0, 0)[0] - 2.0, stage.point(2.0, 0)[1]];
    let to = [press[0] + 480.0, press[1]];
    let (mut scene, gesture) = stage.scene();
    gesture.begin(&mut scene, press, to);
    gesture.drag(&mut scene, to, &|path| (path == "a.mp4").then_some(4.0));
    gesture.finish(&mut scene, Some(to));
    let clip = stage.timeline.clip(id).unwrap();
    assert_eq!((clip.start, clip.duration), (0.0, 3.0));
}

#[test]
fn a_short_clip_is_all_move_body_and_the_gap_between_lanes_is_nothing() {
    let mut timeline = Timeline::empty_project();
    let short = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.2, 0.157, "scene");
    timeline.append_track();
    let mut stage = Stage::new(timeline);
    let before = stage.timeline.clone();
    let hover = |stage: &Stage, pos| {
        hover_clip(&stage.port, &stage.timeline, VIEW, pos).map(|(clip, hit)| (clip.id, hit))
    };
    assert_eq!(
        hover(&stage, stage.center(short)),
        Some((short, ClipHit::Body))
    );

    // One point below the painted clip, still above the next lane.
    let gap = [stage.center(short)[0], VIEW.lanes_top() + TRACK_H + 1.0];
    assert_eq!(hover(&stage, gap), None);
    stage.selected = Some(short);
    stage.click(gap);
    assert_eq!(stage.selected, None);
    assert_eq!(stage.timeline, before);
}

/// A full-length clip flush at zero: its start edge is grabbed from a
/// few points inside the header gutter, and that must still trim.
#[test]
fn a_clip_at_time_zero_trims_from_a_press_just_inside_the_header() {
    let mut timeline = Timeline::empty_project();
    let id = timeline.add_media_clip("full.mp4", ClipKind::Video, 1, 0.0, 20.0, 0.0, "full");
    let mut stage = Stage::new(timeline);
    let press = [VIEW.content_left() - 3.0, 99.0];
    assert!(Gesture::accepts(VIEW, press));
    assert!(!Gesture::accepts(VIEW, [VIEW.content_left() - 40.0, 99.0]));
    stage.drag_through(press, &[[170.0, 99.0], [210.0, 99.0]]);
    let clip = stage.timeline.clip(id).unwrap();
    assert!(clip.start > 0.5, "the trim never began: {}", clip.start);
    assert!((clip.end() - 20.0).abs() < 1e-6);
    assert!((clip.source_in - clip.start).abs() < 1e-9);
}

/// Grabbing six points inside the left edge and dragging 24 moves the
/// edge by the drag (0.5 s), not to the pointer.
#[test]
fn a_left_trim_moves_by_the_drag_and_keeps_the_end_fixed() {
    let (mut stage, id) = clip_fixture();
    let (mut scene, gesture) = stage.scene();
    gesture.begin(&mut scene, [246.0, 99.0], [270.0, 99.0]);
    gesture.drag(&mut scene, [270.0, 99.0], &|_| None);
    assert!(gesture.is_interacting(), "a trim stays active while held");
    gesture.finish(&mut scene, Some([270.0, 99.0]));
    assert!(!gesture.is_interacting());
    let clip = stage.timeline.clip(id).unwrap();
    assert!((clip.start - 2.5).abs() < 0.02, "start {}", clip.start);
    assert!((clip.end() - 8.0).abs() < 1e-6);
    assert!((clip.source_in - 3.5).abs() < 0.02);

    // After another clip, with the magnet on: the release once rippled
    // the trimmed clip back, taking the trim off its end as well.
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 1, 0.0, 10.0, 0.0, "a");
    let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 1, 12.0, 10.0, 0.0, "b");
    let mut stage = Stage::new(timeline);
    let edge = stage.point(12.0, 1);
    stage.drag(edge, [edge[0] + 144.0, edge[1]]);
    let a = stage.timeline.clip(a).unwrap();
    assert_eq!((a.start, a.end()), (0.0, 10.0));
    let b = stage.timeline.clip(b).unwrap();
    assert!((b.start - 15.0).abs() < 0.02, "start {}", b.start);
    assert!((b.end() - 22.0).abs() < 1e-6, "end {}", b.end());
    assert!((b.source_in - 3.0).abs() < 0.02);
}

/// Two clips share the edge at 8 s. A press just right of it trims the
/// right clip; the first-match hit test once always took the left one.
#[test]
fn touching_clips_each_trim_from_their_own_side_of_the_shared_edge() {
    let (mut stage, a, b) = touching_fixture();
    let edge = stage.point(8.0, 1);
    stage.drag([edge[0] + 5.0, edge[1]], [edge[0] + 53.0, edge[1]]);
    assert_eq!(stage.selected, Some(b));
    let (left, right) = (
        stage.timeline.clip(a).unwrap(),
        stage.timeline.clip(b).unwrap(),
    );
    assert_eq!((left.start, left.end()), (2.0, 8.0));
    assert!((right.start - 9.0).abs() < 0.02, "start {}", right.start);
    assert!((right.end() - 14.0).abs() < 1e-6);
    assert!((right.source_in - 1.0).abs() < 0.02);

    let (mut stage, a, b) = touching_fixture();
    stage.drag([edge[0] - 5.0, edge[1]], [edge[0] - 53.0, edge[1]]);
    assert_eq!(stage.selected, Some(a));
    let (left, right) = (
        stage.timeline.clip(a).unwrap(),
        stage.timeline.clip(b).unwrap(),
    );
    assert!((left.end() - 7.0).abs() < 0.02, "end {}", left.end());
    assert_eq!((left.start, right.start, right.end()), (2.0, 8.0, 14.0));
}

#[test]
fn short_clip_edges_still_trim_when_zoomed_in() {
    for left in [true, false] {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 1.0, 0.2, 0.157, "scene");
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            px_per_sec: 300.0,
            magnet: false,
            ..Default::default()
        };
        let edge = if left { 1.0 } else { 1.2 };
        let from = [stage.x(edge) + if left { 3.0 } else { -3.0 }, 49.0];
        let to = [from[0] + if left { 10.0 } else { -10.0 }, from[1]];
        stage.drag(from, to);
        let clip = stage.timeline.clip(id).unwrap();
        let expected_start = if left { 31.0 / 30.0 } else { 1.0 };
        let expected_end = if left { 1.2 } else { 35.0 / 30.0 };
        assert!((clip.start - expected_start).abs() < 1e-9);
        assert!((clip.end() - expected_end).abs() < 1e-9);
        assert!((clip.source_in - (0.157 + expected_start - 1.0)).abs() < 1e-9);
    }
}

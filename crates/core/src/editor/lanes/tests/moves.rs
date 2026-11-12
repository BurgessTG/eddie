//! Body drags: lane changes, new lanes, the magnet and short clips.

use super::*;

#[test]
fn a_lost_pointer_returns_a_moved_clip_to_its_exact_home() {
    let mut timeline = Timeline::new(30.0);
    // An imported start off the frame grid must come back unrounded.
    let id = timeline.add_media_clip("a", ClipKind::Video, 0, 1.2345, 2.0, 0.0, "a");
    let mut stage = Stage::new(timeline);
    let before = stage.timeline.clone();
    let from = stage.point(2.0, 0);
    let to = [from[0] + 200.0, from[1]];
    let (mut scene, gesture) = stage.scene();
    gesture.begin(&mut scene, from, to);
    gesture.drag(&mut scene, to, &|_| None);
    assert_ne!(scene.timeline.clip(id).unwrap().start, 1.2345);
    gesture.finish(&mut scene, None);
    assert_eq!(stage.timeline, before);
}

#[test]
fn a_drop_past_the_last_lane_grows_one_and_a_held_edge_scrolls() {
    let mut timeline = Timeline::new(30.0);
    let id = timeline.add_media_clip("a", ClipKind::Video, 0, 0.0, 2.0, 0.0, "a");
    // Far right, so the lane the clip leaves stays in use.
    timeline.add_media_clip("stays", ClipKind::Video, 0, 40.0, 1.0, 0.0, "s");
    let lanes = timeline.lanes.video;
    let mut stage = Stage::new(timeline);
    let from = stage.point(1.0, 0);
    let below = [from[0], stage.point(0.0, lanes)[1] + TRACK_H];
    let (mut scene, gesture) = stage.scene();
    assert!(!gesture.autoscroll(scene.port, VIEW, [VIEW.right - 4.0, from[1]]));
    gesture.begin(&mut scene, from, below);
    assert!(gesture.autoscroll(scene.port, VIEW, [VIEW.right - 4.0, from[1]]));
    assert_eq!(scene.port.scroll_x, 14.0);
    assert!(gesture.autoscroll(scene.port, VIEW, [VIEW.content_left() + 4.0, from[1]]));
    assert_eq!(scene.port.scroll_x, 0.0);
    gesture.drag(&mut scene, below, &|_| None);
    gesture.finish(&mut scene, Some(below));
    assert_eq!(stage.timeline.lanes.video, lanes + 1);
    assert_eq!(stage.timeline.clip(id).unwrap().track, lanes);
}

#[test]
fn a_release_uses_where_the_pointer_ended_not_where_it_last_paused() {
    let (mut stage, id) = clip_fixture();
    stage.port.magnet = false;
    stage.drag_through([384.0, 99.0], &[[432.0, 99.0], [480.0, 49.0]]);
    let clip = stage.timeline.clip(id).unwrap();
    assert_eq!(
        (clip.track, clip.start, clip.duration, clip.source_in),
        (0, 4.0, 6.0, 3.0)
    );

    for (press_x, step, expected) in [
        (246.0, 24.0, (3.0, 8.0, 4.0)),
        (522.0, -24.0, (2.0, 7.0, 3.0)),
    ] {
        let (mut stage, id) = clip_fixture();
        stage.port.magnet = false;
        stage.drag_through(
            [press_x, 99.0],
            &[[press_x + step, 99.0], [press_x + 2.0 * step, 99.0]],
        );
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!((clip.start, clip.end(), clip.source_in), expected);
    }
}

#[test]
fn a_vertical_drag_changes_lane_and_one_above_the_first_adds_a_lane_on_top() {
    let (mut stage, id) = clip_fixture();
    stage.drag(stage.point(5.0, 1), stage.point(5.0, 0));
    let clip = stage.timeline.clip(id).unwrap();
    assert_eq!(clip.track, 0);
    assert!((clip.start - 2.0).abs() < 0.02, "start {}", clip.start);

    let mut timeline = Timeline::new(30.0);
    let id = timeline.add_media_clip("clip.mp4", ClipKind::Video, 0, 2.0, 6.0, 0.0, "a");
    let other = timeline.add_media_clip("other.mp4", ClipKind::Video, 0, 10.0, 2.0, 0.0, "a");
    let mut stage = Stage::new(timeline);
    // Dropped in the ruler, the only room above the first lane.
    stage.drag(stage.point(5.0, 0), [stage.x(5.0), 10.0]);
    assert_eq!(stage.timeline.lanes.video, 2);
    assert_eq!(stage.timeline.clip(id).unwrap().track, 0);
    assert_eq!(stage.timeline.clip(other).unwrap().track, 1);
}

#[test]
fn a_drop_below_the_view_still_adds_a_lane_and_keeps_the_sideways_move() {
    let (mut stage, id) = clip_fixture();
    stage.port.magnet = false;
    let outside = [480.0, VIEW.bottom + 20.0];
    let (mut scene, gesture) = stage.scene();
    gesture.begin(&mut scene, [384.0, 99.0], [432.0, 99.0]);
    gesture.drag(&mut scene, [432.0, 99.0], &|_| None);
    gesture.drag(&mut scene, outside, &|_| None);
    assert!(gesture.is_interacting(), "a move follows the pointer out");
    assert_eq!(scene.timeline.lanes.video, 2);
    gesture.finish(&mut scene, Some(outside));
    assert!(!gesture.is_interacting());
    assert_eq!(stage.selected, Some(id));
    assert_eq!(stage.timeline.lanes.video, 3);
    let clip = stage.timeline.clip(id).unwrap();
    assert_eq!(
        (clip.track, clip.start, clip.duration, clip.source_in),
        (2, 4.0, 6.0, 3.0)
    );
}

#[test]
fn move_magnet_uses_start_target_when_end_has_no_target() {
    let mut timeline = Timeline::new(30.0);
    let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
    let target = timeline.add_media_clip("target", ClipKind::Video, 1, 8.0, 2.0, 0.0, "target");
    let target_before = timeline.clip(target).unwrap().clone();
    let mut stage = Stage::new(timeline);
    let from = stage.center(id);
    let to = [stage.x(7.9 + 0.5), from[1]];
    stage.drag(from, to);
    assert_eq!(stage.timeline.clip(id).unwrap().start, 8.0);
    assert_eq!(stage.timeline.clip(target).unwrap(), &target_before);
}

#[test]
fn move_magnet_uses_end_target_when_start_has_no_target() {
    let mut timeline = Timeline::new(30.0);
    let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
    timeline.add_media_clip("target", ClipKind::Video, 1, 8.0, 2.0, 0.0, "target");
    let mut stage = Stage::new(timeline);
    let from = stage.center(id);
    let to = [stage.x(6.9 + 0.5), from[1]];
    stage.drag(from, to);
    assert_eq!(stage.timeline.clip(id).unwrap().end(), 8.0);
}

#[test]
fn move_magnet_respects_toggle_threshold_and_nearest_edge() {
    for (magnet, raw, expected) in [
        (true, 7.9, 238.0 / 30.0),
        (false, 7.9, 7.9),
        (true, 7.7, 7.7),
    ] {
        let mut timeline = Timeline::new(30.0);
        let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
        timeline.add_media_clip(
            "target",
            ClipKind::Video,
            1,
            8.0,
            28.0 / 30.0,
            0.0,
            "target",
        );
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            magnet,
            ..Default::default()
        };
        let from = stage.center(id);
        let to = [stage.x(raw + 0.5), from[1]];
        stage.drag(from, to);
        assert!(
            (stage.timeline.clip(id).unwrap().start - expected).abs() < 1e-9,
            "magnet={magnet} raw={raw}"
        );
    }
}

#[test]
fn move_magnet_does_not_snap_to_linked_partner() {
    let mut timeline = Timeline::new(30.0);
    let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 2.0, 0.157, "scene");
    let audio = timeline.detach_audio(id).unwrap();
    let mut stage = Stage::new(timeline);
    let from = stage.center(id);
    stage.drag(from, [from[0] + 7.2, from[1]]);
    assert_eq!(stage.timeline.clip(id).unwrap().start, 65.0 / 30.0);
    assert_eq!(
        stage.timeline.clip(audio).unwrap().start,
        stage.timeline.clip(id).unwrap().start
    );
}

#[test]
fn short_clip_center_moves_without_trimming() {
    for duration in [1.0 / 120.0, 0.1, 0.2, 0.375] {
        let mut timeline = Timeline::new(30.0);
        let id =
            timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, duration, 0.157, "scene");
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(id);
        let selected = stage.drag(from, [from[0] + 48.0, from[1]]);
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!(selected, Some(id));
        assert_eq!(
            (clip.start, clip.duration, clip.source_in),
            (3.0, duration, 0.157),
            "duration={duration}"
        );
    }
}

#[test]
fn short_clip_center_moves_to_existing_lane_without_trimming() {
    for duration in [1.0 / 120.0, 0.1, 0.2, 0.375] {
        let mut timeline = Timeline::new(30.0);
        let id =
            timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, duration, 0.157, "scene");
        timeline.add_media_clip("stays", ClipKind::Video, 0, 40.0, 1.0, 0.0, "s");
        timeline.add_lane(ClipKind::Video, false);
        let mut stage = Stage::new(timeline);
        let from = stage.center(id);
        stage.drag(from, [from[0], from[1] + 50.0]);
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!(
            (clip.track, clip.start, clip.duration, clip.source_in),
            (1, 2.0, duration, 0.157),
            "duration={duration}"
        );
    }
}

#[test]
fn short_clip_center_is_not_stolen_by_neighbor_edge() {
    let mut timeline = Timeline::new(30.0);
    let left = timeline.add_media_clip("left", ClipKind::Video, 0, 0.0, 2.0, 0.0, "left");
    let id = timeline.add_media_clip(
        "insert",
        ClipKind::Video,
        0,
        2.0,
        1.0 / 120.0,
        0.157,
        "insert",
    );
    let right = timeline.add_media_clip(
        "right",
        ClipKind::Video,
        0,
        2.0 + 1.0 / 120.0,
        2.0,
        0.0,
        "right",
    );
    timeline.add_lane(ClipKind::Video, false);
    let before = timeline.clone();
    let mut stage = Stage::new(timeline);
    stage.port = TimelineViewport {
        magnet: false,
        ..Default::default()
    };
    let from = stage.center(id);
    assert_eq!(stage.drag(from, [from[0], from[1] + 50.0]), Some(id));
    assert_eq!(stage.timeline.clip(id).unwrap().track, 1);
    assert_eq!(stage.timeline.clip(id).unwrap().duration, 1.0 / 120.0);
    for other in [left, right] {
        assert_eq!(stage.timeline.clip(other), before.clip(other));
    }
}

#[test]
fn short_clips_of_every_kind_grow_lanes_of_their_kind() {
    for kind in [ClipKind::Video, ClipKind::Audio, ClipKind::Title] {
        for above in [false, true] {
            let mut timeline = Timeline::new(30.0);
            let id = timeline.add_clip("short", kind, 0, 2.0, 0.2, 0.157);
            let other = timeline.add_clip("other", kind, 0, 4.0, 0.4, 1.237);
            let mut stage = Stage::new(timeline);
            let from = stage.center(id);
            let to = [from[0], if above { 10.0 } else { 120.0 }];
            stage.drag(from, to);
            let clip = stage.timeline.clip(id).unwrap();
            assert_eq!(stage.timeline.lanes.count(kind), 2, "{kind:?}");
            assert_eq!(clip.track, if above { 0 } else { 1 });
            assert_eq!(
                (clip.start, clip.duration, clip.source_in),
                (2.0, 0.2, 0.157)
            );
            assert_eq!(
                stage.timeline.clip(other).unwrap().track,
                if above { 1 } else { 0 }
            );
            assert_eq!(stage.timeline.clip(other).unwrap().start, 4.0);
        }
    }
}

#[test]
fn linked_short_clip_lane_change_keeps_its_sound_in_place() {
    // Rows: video 0, video 1 (empty), then the sound's lane. Let go on the
    // empty lane, or past it over the sound's lane, where the picture gets
    // a new lane and the empty one goes: either way it ends on video 1.
    for target in [1, 2] {
        let mut timeline = Timeline::new(30.0);
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.1, 0.157, "scene");
        let audio = timeline.detach_audio(id).unwrap();
        timeline.add_media_clip("stays", ClipKind::Video, 0, 40.0, 1.0, 0.0, "s");
        timeline.add_lane(ClipKind::Video, false);
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        let from = stage.center(id);
        stage.drag(from, [from[0], from[1] + target as f32 * 50.0]);
        let mut expected = before;
        expected.clip_mut(id).unwrap().track = 1;
        assert_eq!(stage.timeline, expected);
        assert_eq!(
            stage.timeline.clip(id).unwrap().start,
            stage.timeline.clip(audio).unwrap().start
        );
    }
}

#[test]
fn a_clip_that_leaves_its_lane_empty_takes_the_lane_away() {
    // Alone on video lane 0 of a full project: dragged past the last video
    // lane it gets a new one, and the lane it left goes.
    let mut timeline = Timeline::empty_project();
    let id = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 2.0, 0.0, "a");
    let other = timeline.add_media_clip("b", ClipKind::Video, 1, 2.0, 2.0, 0.0, "b");
    let mut stage = Stage::new(timeline);
    let from = stage.center(id);
    let below = stage.center(other);
    // Over the sound's lane, well past half a lane below the last video lane.
    stage.drag(from, [below[0], below[1] + TRACK_H + 10.0]);
    let timeline = &stage.timeline;
    assert_eq!(timeline.lanes.video, 2, "video 0 went, video 2 came");
    assert_eq!(
        (
            timeline.clip(other).unwrap().track,
            timeline.clip(id).unwrap().track
        ),
        (0, 1)
    );
    assert_eq!((timeline.lanes.titles, timeline.lanes.audio), (1, 1));
}

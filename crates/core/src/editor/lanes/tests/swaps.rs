//! Drops onto a neighbour: swaps, linked pairs and refused placements.

use super::*;

#[test]
fn a_drop_on_a_neighbour_swaps_places_and_a_drop_across_two_returns_home() {
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 0, 2.0, 6.0, 0.0, "a");
    let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 0, 10.0, 6.0, 0.0, "b");
    let mut stage = Stage::new(timeline);
    // Held at 5 s, dropped with the pointer at 13 s, inside the other.
    stage.drag(stage.point(5.0, 0), stage.point(13.0, 0));
    assert!((stage.timeline.clip(a).unwrap().start - 10.0).abs() < 0.02);
    assert!((stage.timeline.clip(b).unwrap().start - 2.0).abs() < 0.02);

    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 0, 2.0, 6.0, 0.0, "a");
    let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 0, 10.0, 2.0, 0.0, "b");
    let c = timeline.add_media_clip("c.mp4", ClipKind::Video, 0, 12.0, 2.0, 0.0, "c");
    let mut stage = Stage::new(timeline);
    // Dropped from 9 s it would cover both others: no single swap.
    stage.drag(stage.point(5.0, 0), stage.point(12.0, 0));
    assert!((stage.timeline.clip(a).unwrap().start - 2.0).abs() < 0.02);
    assert_eq!(stage.timeline.clip(b).unwrap().start, 10.0);
    assert_eq!(stage.timeline.clip(c).unwrap().start, 12.0);
}

#[test]
fn move_drop_adjacent_unequal_clips_preserves_outer_span() {
    for (left_duration, right_duration, drag_left) in [
        (0.2, 0.4, true),
        (0.2, 0.4, false),
        (0.4, 0.2, true),
        (0.4, 0.2, false),
    ] {
        let mut timeline = Timeline::empty_project();
        let left = timeline.add_media_clip(
            "left",
            ClipKind::Video,
            0,
            2.0,
            left_duration,
            0.157,
            "left",
        );
        let right = timeline.add_media_clip(
            "right",
            ClipKind::Video,
            0,
            2.0 + left_duration,
            right_duration,
            1.237,
            "right",
        );
        let tail = timeline.add_media_clip(
            "tail",
            ClipKind::Video,
            0,
            2.0 + left_duration + right_duration,
            0.5,
            4.0,
            "tail",
        );
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            px_per_sec: 240.0,
            magnet: false,
            ..Default::default()
        };
        let id = if drag_left { left } else { right };
        let from = stage.center(id);
        let raw = if drag_left { 2.0 + right_duration } else { 2.0 };
        let to = [
            stage.x(raw + stage.timeline.clip(id).unwrap().duration / 2.0),
            from[1],
        ];
        stage.drag(from, to);
        assert!(
            (stage.timeline.clip(left).unwrap().start - (2.0 + right_duration)).abs() < 1e-9,
            "left_duration={left_duration} right_duration={right_duration} drag_left={drag_left}"
        );
        assert_eq!(stage.timeline.clip(right).unwrap().start, 2.0);
        for id in [left, right] {
            let clip = stage.timeline.clip(id).unwrap();
            let original = before.clip(id).unwrap();
            assert_eq!(
                (clip.duration, clip.source_in),
                (original.duration, original.source_in)
            );
        }
        assert_eq!(stage.timeline.clip(tail), before.clip(tail));
    }
}

#[test]
fn move_drop_adjacent_with_gap_preserves_gap_and_neighbors() {
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 0.4, 0.0, "a");
    let b = timeline.add_media_clip("b", ClipKind::Video, 0, 2.6, 0.2, 0.0, "b");
    let tail = timeline.add_media_clip("tail", ClipKind::Video, 0, 2.8, 1.0, 0.0, "tail");
    let tail_before = timeline.clip(tail).unwrap().clone();
    let mut stage = Stage::new(timeline);
    stage.port = TimelineViewport {
        px_per_sec: 120.0,
        magnet: false,
        ..Default::default()
    };
    let from = stage.center(a);
    let to = [stage.x(2.6), from[1]];
    stage.drag(from, to);
    assert!((stage.timeline.clip(a).unwrap().start - 2.4).abs() < 1e-9);
    assert!((stage.timeline.clip(a).unwrap().end() - 2.8).abs() < 1e-9);
    assert_eq!(stage.timeline.clip(b).unwrap().start, 2.0);
    assert!(
        (stage.timeline.clip(a).unwrap().start - stage.timeline.clip(b).unwrap().end() - 0.2).abs()
            < 1e-9
    );
    assert_eq!(stage.timeline.clip(tail).unwrap(), &tail_before);
}

#[test]
fn move_drop_blocked_on_another_lane_reverts_exact_fractional_start() {
    for linked in [false, true] {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.157, 0.418, 0.157, "scene");
        if linked {
            timeline.detach_audio(id).unwrap();
        }
        timeline.add_media_clip("blocker", ClipKind::Video, 2, 5.0, 2.0, 0.0, "blocker");
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            px_per_sec: 120.0,
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(id);
        let to = [stage.x(5.0 + 0.209), 149.0];
        stage.drag(from, to);
        assert_eq!(
            stage.timeline, before,
            "rejected drop must restore exact placement (linked={linked})"
        );
    }
}

#[test]
fn move_drop_with_only_linked_audio_collision_reverts() {
    let mut timeline = Timeline::empty_project();
    let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 1.0, 0.157, "scene");
    timeline.detach_audio(id).unwrap();
    timeline.add_media_clip("blocker", ClipKind::Audio, 1, 4.0, 1.0, 0.0, "blocker");
    let before = timeline.clone();
    let mut stage = Stage::new(timeline);
    stage.port = TimelineViewport {
        magnet: false,
        ..Default::default()
    };
    let from = stage.center(id);
    stage.drag(from, [from[0] + 96.0, from[1]]);
    assert_eq!(stage.timeline, before);
}

#[test]
fn move_drop_linked_swap_checks_both_linked_lanes_atomically() {
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 1.0, 0.157, "a");
    let b = timeline.add_media_clip("b", ClipKind::Video, 0, 4.0, 1.0, 1.237, "b");
    timeline.detach_audio(a).unwrap();
    let b_audio = timeline.detach_audio(b).unwrap();
    timeline.set_clip_track(b_audio, 2);
    timeline.add_media_clip("blocker", ClipKind::Audio, 1, 4.0, 1.0, 0.0, "blocker");
    let before = timeline.clone();
    let mut stage = Stage::new(timeline);
    stage.port = TimelineViewport {
        magnet: false,
        ..Default::default()
    };
    let from = stage.center(a);
    stage.drag(from, [from[0] + 96.0, from[1]]);
    assert_eq!(
        stage.timeline, before,
        "a blocked companion must reject the entire swap"
    );
}

#[test]
fn move_drop_linked_swap_keeps_all_four_ranges() {
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 0.2, 0.157, "a");
    let b = timeline.add_media_clip("b", ClipKind::Video, 0, 2.2, 0.4, 1.237, "b");
    let a_audio = timeline.detach_audio(a).unwrap();
    let b_audio = timeline.detach_audio(b).unwrap();
    let before = timeline.clone();
    let mut stage = Stage::new(timeline);
    stage.port = TimelineViewport {
        px_per_sec: 240.0,
        magnet: false,
        ..Default::default()
    };
    let from = stage.center(a);
    let to = [stage.x(2.5), from[1]];
    stage.drag(from, to);
    for (id, expected_start) in [(a, 2.4), (a_audio, 2.4), (b, 2.0), (b_audio, 2.0)] {
        let clip = stage.timeline.clip(id).unwrap();
        let original = before.clip(id).unwrap();
        assert!((clip.start - expected_start).abs() < 1e-9);
        assert_eq!(
            (clip.duration, clip.source_in, clip.track, clip.linked_clip),
            (
                original.duration,
                original.source_in,
                original.track,
                original.linked_clip
            )
        );
    }
}

#[test]
fn move_drop_new_lane_with_linked_collision_reverts_without_growing() {
    let mut timeline = Timeline::empty_project();
    let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 1.0, 0.157, "scene");
    timeline.detach_audio(id).unwrap();
    timeline.add_media_clip("blocker", ClipKind::Audio, 1, 4.0, 1.0, 0.0, "blocker");
    let before = timeline.clone();
    let mut stage = Stage::new(timeline);
    stage.port = TimelineViewport {
        magnet: false,
        ..Default::default()
    };
    let from = stage.center(id);
    let to = [from[0] + 96.0, 170.0];
    stage.drag(from, to);
    assert_eq!(stage.timeline, before);
}

#[test]
fn dense_short_timeline_default_zoom_reorders_only_the_adjacent_pair() {
    for magnet in [false, true] {
        for drag_left in [false, true] {
            for (left_duration, right_duration) in [(0.2, 0.4), (0.4, 0.2)] {
                let mut timeline = Timeline::empty_project();
                for i in 0..40 {
                    timeline.add_media_clip(
                        "prefix",
                        ClipKind::Video,
                        0,
                        i as f64 * 0.1,
                        0.1,
                        i as f64 * 0.1,
                        "scene",
                    );
                }
                let left = timeline.add_media_clip(
                    "left",
                    ClipKind::Video,
                    0,
                    4.0,
                    left_duration,
                    0.157,
                    "left",
                );
                let right = timeline.add_media_clip(
                    "right",
                    ClipKind::Video,
                    0,
                    4.0 + left_duration,
                    right_duration,
                    1.237,
                    "right",
                );
                for i in 0..40 {
                    timeline.add_media_clip(
                        "tail",
                        ClipKind::Video,
                        0,
                        4.6 + i as f64 * 0.1,
                        0.1,
                        i as f64 * 0.1,
                        "scene",
                    );
                }
                let before = timeline.clone();
                let mut stage = Stage::new(timeline);
                stage.port = TimelineViewport {
                    magnet,
                    ..Default::default()
                };
                let id = if drag_left { left } else { right };
                let from = stage.center(id);
                let raw = if drag_left { 4.0 + right_duration } else { 4.0 };
                let to = [
                    stage.x(raw + stage.timeline.clip(id).unwrap().duration / 2.0),
                    from[1],
                ];
                assert_eq!(stage.drag(from, to), Some(id));
                assert!(
                    (stage.timeline.clip(left).unwrap().start - (4.0 + right_duration)).abs()
                        < 1e-9
                );
                assert_eq!(stage.timeline.clip(right).unwrap().start, 4.0);
                assert_eq!(stage.timeline.clips.len(), 82);
                for clip in &stage.timeline.clips {
                    let mut expected = before.clip(clip.id).unwrap().clone();
                    if clip.id == left || clip.id == right {
                        expected.start = clip.start;
                    }
                    assert_eq!(clip, &expected, "no ripple, trim or source change");
                }
            }
        }
    }
}

#[test]
fn unequal_adjacent_center_drop_does_not_collide_with_unrelated_neighbors() {
    for magnet in [false, true] {
        for drag_left in [false, true] {
            let (left_duration, right_duration) = if drag_left { (0.6, 0.2) } else { (0.2, 0.6) };
            let mut timeline = Timeline::empty_project();
            timeline.add_media_clip("prefix", ClipKind::Video, 0, 1.5, 0.5, 0.0, "prefix");
            let left = timeline.add_media_clip(
                "left",
                ClipKind::Video,
                0,
                2.0,
                left_duration,
                0.157,
                "left",
            );
            let right = timeline.add_media_clip(
                "right",
                ClipKind::Video,
                0,
                2.0 + left_duration,
                right_duration,
                1.237,
                "right",
            );
            timeline.add_media_clip("tail", ClipKind::Video, 0, 2.8, 0.5, 0.0, "tail");
            let before = timeline.clone();
            let mut stage = Stage::new(timeline);
            stage.port = TimelineViewport {
                magnet,
                ..Default::default()
            };
            let (dragged, target) = if drag_left {
                (left, right)
            } else {
                (right, left)
            };
            let from = stage.center(dragged);
            let to = stage.center(target);
            stage.drag(from, to);
            assert!(
                (stage.timeline.clip(left).unwrap().start - (2.0 + right_duration)).abs() < 1e-9,
                "magnet={magnet} drag_left={drag_left}"
            );
            assert_eq!(stage.timeline.clip(right).unwrap().start, 2.0);
            for clip in &stage.timeline.clips {
                let mut expected = before.clip(clip.id).unwrap().clone();
                if clip.id == left || clip.id == right {
                    expected.start = clip.start;
                }
                assert_eq!(clip, &expected);
            }
        }
    }
}

/// A clip a little over a second long, as media lengths are, on lane 0
/// at 2 s, and a neighbour at 6 s on the same lane.
fn butting_fixture(magnet: bool) -> (Stage, ClipId, ClipId) {
    let mut timeline = Timeline::empty_project();
    let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 0, 2.0, 1.2345, 0.0, "a");
    let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 0, 6.0, 2.0, 0.0, "b");
    let mut stage = Stage::new(timeline);
    stage.port.magnet = magnet;
    (stage, a, b)
}

#[test]
fn the_magnet_butts_a_clip_against_its_neighbour_without_swapping() {
    let (mut stage, a, b) = butting_fixture(true);
    let from = stage.center(a);
    // Its end let go 0.03 s short of the neighbour, inside the snap.
    let shift = 6.0 - 0.03 - stage.timeline.clip(a).unwrap().end();
    stage.drag(from, [stage.x(2.0 + 1.2345 / 2.0 + shift), from[1]]);
    assert_eq!(
        stage.timeline.clip(b).unwrap().start,
        6.0,
        "the neighbour stays"
    );
    assert!(
        (stage.timeline.clip(a).unwrap().end() - 6.0).abs() < 1e-9,
        "flush, not a frame in"
    );
}

#[test]
fn a_clip_dragged_a_little_into_its_neighbour_stops_against_it() {
    for magnet in [false, true] {
        let (mut stage, a, b) = butting_fixture(magnet);
        let from = stage.center(a);
        // 0.3 s into the neighbour: far less than half of either clip.
        let shift = 6.3 - stage.timeline.clip(a).unwrap().end();
        let to = [stage.x(2.0 + 1.2345 / 2.0 + shift), from[1]];
        stage.drag_through(from, &[to]);
        assert_eq!(
            stage.timeline.clip(b).unwrap().start,
            6.0,
            "magnet={magnet}"
        );
        assert!(
            (stage.timeline.clip(a).unwrap().end() - 6.0).abs() < 1e-9,
            "magnet={magnet}"
        );

        // Carried well over it, they trade places.
        let (mut stage, a, b) = butting_fixture(magnet);
        let from = stage.center(a);
        stage.drag(from, stage.point(7.0, 0));
        assert_eq!(
            stage.timeline.clip(b).unwrap().start,
            2.0,
            "magnet={magnet}"
        );
        assert!((stage.timeline.clip(a).unwrap().end() - 8.0).abs() < 1e-9);
    }
}

#[test]
fn a_small_overlap_on_another_lane_also_stops_against_the_clip_there() {
    let (mut stage, a, b) = butting_fixture(false);
    stage.timeline.set_clip_track(b, 1);
    let from = stage.center(a);
    let shift = 6.3 - stage.timeline.clip(a).unwrap().end();
    stage.drag(
        from,
        [stage.x(2.0 + 1.2345 / 2.0 + shift), stage.point(0.0, 1)[1]],
    );
    let clip = stage.timeline.clip(a).unwrap();
    assert_eq!(clip.track, 1);
    assert!((clip.end() - 6.0).abs() < 1e-9);
    assert_eq!(stage.timeline.clip(b).unwrap().start, 6.0);
}

use super::*;

#[test]
fn snap_aligns_to_frame_grid() {
    let tl = Timeline::new(30.0);
    assert!((tl.snap(1.01) - 1.0).abs() < 1e-9);
    assert!((tl.snap(1.02) - 31.0 / 30.0).abs() < 1e-9);
}

#[test]
fn media_placement_preserves_source_intervals() {
    let mut tl = Timeline::new(30.0);
    for kind in [ClipKind::Video, ClipKind::Audio] {
        for (start, duration, source_in) in [
            (49.0 / 60.0, 1.235 - 49.0 / 60.0, 49.0 / 60.0),
            (0.0, 1.0 / 120.0, 17.0 / 120.0),
            (2.157, 0.001, 0.157),
        ] {
            let id = tl.add_media_clip(
                "scene",
                kind,
                0,
                start,
                duration,
                source_in,
                "/media/scene.mp4",
            );
            let clip = tl.clip(id).unwrap();
            assert_eq!(
                (clip.start, clip.duration, clip.source_in),
                (start, duration, source_in)
            );
            assert_eq!(clip.end(), start + duration);
        }
    }

    let title = tl.add_clip("title", ClipKind::Title, 1, 1.02, 0.01, 0.0);
    assert_eq!(tl.clip(title).unwrap().start, 31.0 / 30.0);
    assert_eq!(tl.clip(title).unwrap().duration, 1.0 / 30.0);
    tl.move_clip(tl.clips[0].id, 2.02);
    assert_eq!(tl.clips[0].start, 61.0 / 30.0);
}

#[test]
fn fractional_media_keeps_coverage_and_linked_audio_through_edits() {
    let mut tl = Timeline::empty_project();
    let video = tl.add_media_clip(
        "scene",
        ClipKind::Video,
        0,
        0.817,
        0.418,
        2.135,
        "/media/scene.mp4",
    );
    let audio = tl.detach_audio(video).unwrap();
    for id in [video, audio] {
        let clip = tl.clip(id).unwrap();
        assert_eq!(
            (clip.start, clip.duration, clip.source_in),
            (0.817, 0.418, 2.135)
        );
    }

    let right_video = tl.split_at(video, 1.0).unwrap();
    let right_audio = tl.clip(right_video).unwrap().linked_clip.unwrap();
    for (left, right) in [(video, right_video), (audio, right_audio)] {
        let left = tl.clip(left).unwrap();
        let right = tl.clip(right).unwrap();
        assert_eq!(left.end(), 1.0);
        assert_eq!(right.start, left.end());
        assert!((right.end() - 1.235).abs() < 1e-12);
        assert!((left.duration + right.duration - 0.418).abs() < 1e-12);
        assert_eq!(right.source_in, left.source_in + left.duration);
    }

    let tail = tl.add_media_clip(
        "tail",
        ClipKind::Video,
        0,
        2.0,
        0.007,
        4.0,
        "/media/scene.mp4",
    );
    let tail_audio = tl.detach_audio(tail).unwrap();
    tl.close_gaps();
    for id in [tail, tail_audio] {
        let clip = tl.clip(id).unwrap();
        assert_eq!(clip.start, tl.clip(right_video).unwrap().end());
        assert_eq!(clip.duration, 0.007);
        assert_eq!(clip.source_in, 4.0);
    }
    let unchanged = tl.clone();
    tl.close_gaps();
    assert_eq!(tl, unchanged, "closing linked lanes again is a no-op");
}

#[test]
fn move_clamps_to_zero() {
    let mut tl = Timeline::demo();
    let id = tl.clips[0].id;
    tl.move_clip(id, -5.0);
    assert_eq!(tl.clip(id).unwrap().start, 0.0);
}

#[test]
fn adjacent_swaps_preserve_fractional_span_gap_and_linked_coverage() {
    for left_duration in [1.0 / 120.0, 0.157, 0.418] {
        for right_duration in [0.001, 0.1, 7.0 / 30.0] {
            for gap in [0.0, 0.23] {
                for reverse in [false, true] {
                    let mut tl = Timeline::empty_project();
                    let left = tl.add_media_clip(
                        "left",
                        ClipKind::Video,
                        0,
                        0.817,
                        left_duration,
                        2.135,
                        "left",
                    );
                    let right = tl.add_media_clip(
                        "right",
                        ClipKind::Video,
                        0,
                        0.817 + left_duration + gap,
                        right_duration,
                        4.237,
                        "right",
                    );
                    let left_audio = tl.detach_audio(left).unwrap();
                    let right_audio = tl.detach_audio(right).unwrap();
                    let outer_end = tl.clip(right).unwrap().end();
                    let prefix =
                        tl.add_media_clip("prefix", ClipKind::Video, 0, 0.0, 0.817, 0.0, "prefix");
                    let tail =
                        tl.add_media_clip("tail", ClipKind::Video, 0, outer_end, 0.1, 0.0, "tail");
                    let before = tl.clone();
                    let (first, second) = if reverse {
                        (right, left)
                    } else {
                        (left, right)
                    };
                    assert!(
                        tl.swap_clips(first, second),
                        "left={left_duration} right={right_duration} gap={gap} reverse={reverse}"
                    );
                    for (id, expected_start) in [
                        (left, outer_end - left_duration),
                        (left_audio, outer_end - left_duration),
                        (right, 0.817),
                        (right_audio, 0.817),
                    ] {
                        let clip = tl.clip(id).unwrap();
                        assert!((clip.start - expected_start).abs() < 1e-12);
                        let mut expected = before.clip(id).unwrap().clone();
                        expected.start = clip.start;
                        assert_eq!(clip, &expected, "only start may change");
                    }
                    assert!((tl.clip(left).unwrap().end() - outer_end).abs() < 1e-12);
                    assert!(
                        (tl.clip(left).unwrap().start - tl.clip(right).unwrap().end() - gap).abs()
                            < 1e-12
                    );
                    assert_eq!(tl.clip(prefix), before.clip(prefix));
                    assert_eq!(tl.clip(tail), before.clip(tail));
                    assert_eq!(tl.lanes, before.lanes);
                    assert!(tl.swap_clips(first, second));
                    for clip in &tl.clips {
                        assert!(
                            (clip.start - before.clip(clip.id).unwrap().start).abs() < 1e-12,
                            "swapping back restores the original order"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn swap_rejects_collisions_on_either_companion_lane_atomically() {
    for block_left in [false, true] {
        let mut tl = Timeline::empty_project();
        let left = tl.add_media_clip("left", ClipKind::Video, 0, 2.0, 0.4, 0.157, "left");
        let right = tl.add_media_clip("right", ClipKind::Video, 0, 2.4, 0.2, 1.237, "right");
        tl.detach_audio(left).unwrap();
        let right_audio = tl.detach_audio(right).unwrap();
        tl.add_lane(ClipKind::Audio, false);
        assert!(tl.set_clip_track(right_audio, 1));
        let (track, start) = if block_left { (0, 2.4) } else { (1, 2.0) };
        tl.add_media_clip(
            "blocker",
            ClipKind::Audio,
            track,
            start,
            0.2,
            0.0,
            "blocker",
        );
        let before = tl.clone();
        assert!(!tl.swap_clips(left, right));
        assert_eq!(tl, before);
        assert!(!tl.swap_clips(right, left));
        assert_eq!(tl, before);
    }
}

#[test]
fn nonadjacent_swaps_leave_middle_clips_unchanged_and_reject_collisions() {
    for duration in [0.4, 1.0] {
        let mut tl = Timeline::empty_project();
        let left = tl.add_media_clip("left", ClipKind::Video, 0, 2.0, duration, 0.0, "left");
        let right = tl.add_media_clip("right", ClipKind::Video, 0, 5.0, 0.4, 0.0, "right");
        let middle = tl.add_media_clip("middle", ClipKind::Video, 0, 3.0, 1.0, 0.0, "middle");
        let tail = tl.add_media_clip("tail", ClipKind::Video, 0, 5.4, 1.0, 0.0, "tail");
        let before = tl.clone();
        if duration == 0.4 {
            assert!(tl.swap_clips(left, right));
            assert_eq!(tl.clip(left).unwrap().start, 5.0);
            assert_eq!(tl.clip(right).unwrap().start, 2.0);
            assert_eq!(tl.clip(middle), before.clip(middle));
            assert_eq!(tl.clip(tail), before.clip(tail));
        } else {
            assert!(!tl.swap_clips(left, right));
            assert_eq!(tl, before);
        }
    }
}

#[test]
fn swap_rejects_invalid_pairs_without_mutating() {
    let mut tl = Timeline::empty_project();
    let video = tl.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.4, 0.157, "scene");
    let audio = tl.detach_audio(video).unwrap();
    let before = tl.clone();
    for (first, second) in [(video, video), (video, audio), (video, 99), (99, video)] {
        assert!(!tl.swap_clips(first, second));
        assert_eq!(tl, before);
    }
}

#[test]
fn split_preserves_total_coverage() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 10.0, 5.0);
    let right = tl.split_at(id, 6.0).expect("split inside clip");
    let left = tl.clip(id).unwrap().clone();
    let right = tl.clip(right).unwrap().clone();
    assert_eq!(left.end(), 6.0);
    assert_eq!(right.start, 6.0);
    assert_eq!(right.end(), 12.0);
    assert_eq!(right.source_in, 9.0);
}

#[test]
fn split_outside_clip_is_noop() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 4.0, 0.0);
    assert!(tl.split_at(id, 99.0).is_none());
    assert_eq!(tl.clips.len(), 1);
}

#[test]
fn timecode_formats_frames() {
    assert_eq!(format_timecode(0.0, 30.0), "00:00:00:00");
    assert_eq!(format_timecode(61.5, 30.0), "00:01:01:15");
    assert_eq!(format_timecode(3661.0, 25.0), "01:01:01:00");
}

#[test]
fn demo_ends_at_last_clip() {
    assert_eq!(Timeline::demo().end(), 21.0);
}

#[test]
fn trim_left_moves_source_in_with_start() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 10.0, 5.0);
    tl.trim_left(id, 4.0);
    let clip = tl.clip(id).unwrap();
    assert_eq!(clip.start, 4.0);
    assert_eq!(clip.source_in, 7.0);
    assert_eq!(clip.end(), 12.0);
}

#[test]
fn trim_left_clamps_at_source_zero() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    let id = tl.add_clip("a.mp4", ClipKind::Video, track, 6.0, 4.0, 1.0);
    tl.trim_left(id, 0.0);
    let clip = tl.clip(id).unwrap();
    // Can only extend 1s earlier before the source runs out.
    assert_eq!(clip.start, 5.0);
    assert_eq!(clip.source_in, 0.0);
    assert_eq!(clip.duration, 5.0);
}

#[test]
fn trim_right_changes_duration_only() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    let id = tl.add_clip("a.mp4", ClipKind::Video, track, 2.0, 10.0, 5.0);
    tl.trim_right(id, 8.0);
    let clip = tl.clip(id).unwrap();
    assert_eq!(clip.duration, 6.0);
    assert_eq!(clip.source_in, 5.0);
}

#[test]
fn close_gaps_removes_empty_time_on_every_lane_together() {
    let mut tl = Timeline::new(30.0);
    let main = 0;
    let other = 0;
    let captions = 0;
    tl.add_clip("a.mp4", ClipKind::Video, main, 1.0, 2.0, 0.0);
    let b = tl.add_clip("b.mp4", ClipKind::Video, main, 5.0, 2.0, 0.0);
    let c = tl.add_clip("c.mp4", ClipKind::Video, main, 10.0, 1.0, 0.0);
    // Audio on another lane bridges 7-8, so only 3-5 and 8-10 are empty.
    let bridge = tl.add_clip("x.wav", ClipKind::Audio, other, 7.0, 1.0, 0.0);
    // Captions over b and c, and a title alone in the 8-10 gap.
    let over_b = tl.add_clip("over b", ClipKind::Title, captions, 5.5, 1.0, 0.0);
    let over_c = tl.add_clip("over c", ClipKind::Title, captions, 10.2, 0.5, 0.0);
    let in_gap = tl.add_clip("in gap", ClipKind::Title, captions, 9.0, 0.5, 0.0);
    tl.close_gaps();
    let start = |id| tl.clip(id).unwrap().start;
    // The leading gap before the first media clip stays.
    assert_eq!(start(b), 3.0);
    assert_eq!(start(bridge), 5.0);
    assert_eq!(start(c), 6.0);
    assert_eq!(start(over_b) - start(b), 0.5);
    assert!((start(over_c) - start(c) - 0.2).abs() < 1e-9);
    assert_eq!(start(in_gap), 6.0);
}

#[test]
fn close_gaps_never_stacks_titles_on_one_lane() {
    let mut tl = Timeline::new(30.0);
    let main = 0;
    let captions = 0;
    tl.add_clip("a.mp4", ClipKind::Video, main, 0.0, 5.0, 0.0);
    let b = tl.add_clip("b.mp4", ClipKind::Video, main, 7.0, 3.0, 0.0);
    // A caption overhanging a's end, one alone in the 5-7 gap and one
    // over b: closing the gap brings all three to within 5-6.
    let overhang = tl.add_clip("overhang", ClipKind::Title, captions, 4.5, 1.0, 0.0);
    let in_gap = tl.add_clip("in gap", ClipKind::Title, captions, 6.0, 0.5, 0.0);
    let over_b = tl.add_clip("over b", ClipKind::Title, captions, 7.0, 1.0, 0.0);
    // A title on a second title lane inside the gap.
    let on_main = tl.add_clip("on main", ClipKind::Title, 1, 5.5, 1.0, 0.0);
    tl.close_gaps();
    let clip = |id| tl.clip(id).unwrap();
    assert_eq!(clip(b).start, 5.0);
    // Times are the ones the pictures moved by.
    assert_eq!(clip(overhang).start, 4.5);
    assert_eq!(clip(in_gap).start, 5.0);
    assert_eq!(clip(over_b).start, 5.0);
    assert_eq!(clip(on_main).start, 5.0);
    assert_eq!((clip(b).track, clip(overhang).track), (main, captions));
    for a in &tl.clips {
        for other in &tl.clips {
            let overlap = a.start < other.end() && other.start < a.end();
            assert!(
                a.id == other.id || !a.same_lane(other) || !overlap,
                "{} and {} share lane {}",
                a.name,
                other.name,
                a.track
            );
        }
    }
}

#[test]
fn lane_overlaps_lists_clips_sharing_a_lane_for_a_frame_or_more() {
    let mut tl = Timeline::new(30.0);
    let lane = 0;
    let other = 1;
    let a = tl.add_clip("a", ClipKind::Video, lane, 0.0, 5.0, 0.0);
    // Half a frame over a's end: a grid edge against an off-grid one.
    tl.add_clip("abuts", ClipKind::Video, lane, 5.0 - 0.5 / 30.0, 2.0, 0.0);
    tl.add_clip("other lane", ClipKind::Video, other, 1.0, 2.0, 0.0);
    assert_eq!(tl.lane_overlaps(), vec![]);
    let inside = tl.add_clip("inside", ClipKind::Video, lane, 1.0, 0.5, 0.0);
    let late = tl.add_clip("late", ClipKind::Video, lane, 4.0, 0.9, 0.0);
    // A title and a sound numbered the same sit on lanes of their own.
    tl.add_clip("title", ClipKind::Title, lane, 0.0, 5.0, 0.0);
    tl.add_clip("sound", ClipKind::Audio, lane, 0.0, 5.0, 0.0);
    assert_eq!(tl.lane_overlaps(), vec![(a, inside), (a, late)]);
    assert!(tl.in_supported_range());
    tl.move_clip(a, 1e12);
    assert!(!tl.in_supported_range());
    assert!(!Timeline::new(0.0).in_supported_range());
    assert!(!Timeline::new(f64::NAN).in_supported_range());
}

#[test]
fn trims_never_pass_the_requested_edge() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    // Media adds keep exact, off-grid edges.
    let id = tl.add_media_clip("a", ClipKind::Video, track, 11.01, 5.0, 3.0, "a.mp4");
    // The caller clamped to media that ends at 16.05: the edge takes the
    // frame before it (16.0333), not the nearer frame after it.
    tl.trim_right(id, 16.05);
    let end = 16.0 + 1.0 / 30.0;
    assert!((tl.clip(id).unwrap().end() - end).abs() < 1e-9);
    // Extending toward a neighbour that starts where this clip already
    // ends off the grid changes nothing instead of shortening it.
    let before = tl.clip(id).unwrap().clone();
    tl.trim_right(id, before.end());
    assert_eq!(tl.clip(id).unwrap(), &before);

    // Left edge: a previous clip ends at 10.005; the edge stops at the
    // first frame after it.
    tl.trim_left(id, 10.005);
    let clip = tl.clip(id).unwrap().clone();
    assert!(clip.start >= 10.005 && (clip.start - 10.0 - 1.0 / 30.0).abs() < 1e-9);
    assert!((clip.source_in - (3.0 - (11.01 - clip.start))).abs() < 1e-9);
    assert!((clip.end() - end).abs() < 1e-9);
    // Dragging left against a neighbour ending 5 ms before an off-grid
    // start must not move the edge right.
    let id = tl.add_media_clip("b", ClipKind::Video, track, 21.01, 2.0, 3.0, "b.mp4");
    tl.trim_left(id, 21.005);
    assert_eq!(tl.clip(id).unwrap().start, 21.01);
    // An off-grid clip at the start of its media stays exactly there.
    let id = tl.add_media_clip("c", ClipKind::Video, track, 30.013, 2.0, 0.0, "c.mp4");
    tl.trim_left(id, 25.0);
    let clip = tl.clip(id).unwrap();
    assert_eq!(
        (clip.start, clip.source_in, clip.duration),
        (30.013, 0.0, 2.0)
    );
    // Not-a-number requests are ignored.
    tl.trim_left(id, f64::NAN);
    tl.trim_right(id, f64::NAN);
    tl.move_clip(id, f64::NAN);
    assert_eq!(tl.clip(id).unwrap().start, 30.013);
}

#[test]
fn a_title_extends_earlier_and_keeps_its_word_timing() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    let id = tl.add_clip("two words", ClipKind::Title, track, 5.0, 2.0, 0.0);
    tl.clip_mut(id).unwrap().words = vec![0.25, 1.0];
    tl.trim_left(id, 4.0);
    let clip = tl.clip(id).unwrap();
    assert_eq!((clip.start, clip.end()), (4.0, 7.0));
    assert_eq!(clip.word_times(), Some(vec![5.25, 6.0]));
    // Splitting inside the part added before the first word keeps
    // both pieces on the same clock.
    let right = tl.split_at(id, 4.5).unwrap();
    assert_eq!(tl.clip(right).unwrap().word_times(), Some(vec![5.25, 6.0]));
}

#[test]
fn a_zero_frame_rate_formats_without_dividing_by_zero() {
    assert_eq!(format_timecode(3.0, 0.0), "00:00:00:00");
    let (fade_in, fade_out) = fit_fades(4.0, 1e308, 1e308);
    assert_eq!((fade_in, fade_out), (2.0, 2.0));
}

#[test]
fn split_keeps_each_fade_on_its_own_edge() {
    let mut tl = Timeline::empty_project();
    let id = tl.add_media_clip("m", ClipKind::Audio, 0, 0.0, 4.0, 0.0, "m.mp3");
    let clip = tl.clip_mut(id).unwrap();
    (clip.fade_in, clip.fade_out) = (1.0, 2.0);
    let right = tl.split_at(id, 1.5).unwrap();
    let (left, right) = (tl.clip(id).unwrap(), tl.clip(right).unwrap());
    assert_eq!((left.fade_in, left.fade_out), (1.0, 0.0));
    assert_eq!((right.fade_in, right.fade_out), (0.0, 2.0));
    assert_eq!(fit_fades(2.0, 3.0, 1.0), (1.5, 0.5));
    assert_eq!(fit_fades(2.0, f64::NAN, -1.0), (0.0, 0.0));
}

#[test]
fn an_empty_project_has_one_lane_of_each_kind() {
    let tl = Timeline::empty_project();
    let one = Lanes {
        titles: 1,
        video: 1,
        audio: 1,
    };
    assert_eq!(tl.lanes, one);
    assert!(tl.clips.is_empty());
    // Titles on top, then video, then sound.
    assert_eq!(
        (0..3).map(|row| tl.lane_at_row(row)).collect::<Vec<_>>(),
        [
            Some((ClipKind::Title, 0)),
            Some((ClipKind::Video, 0)),
            Some((ClipKind::Audio, 0))
        ]
    );
}

#[test]
fn add_clip_grows_its_kinds_lanes_to_cover_its_index() {
    let mut tl = Timeline::empty_project();
    tl.add_clip("a.mp4", ClipKind::Video, 2, 0.0, 2.0, 0.0);
    assert_eq!(
        tl.lanes,
        Lanes {
            titles: 1,
            video: 3,
            audio: 1
        }
    );
    assert_eq!(tl.row(ClipKind::Audio, 0), 4);
}

#[test]
fn a_lane_added_on_top_moves_only_its_kinds_clips_down() {
    let mut tl = Timeline::empty_project();
    let a = tl.add_clip("a.mp4", ClipKind::Video, 0, 0.0, 2.0, 0.0);
    let b = tl.add_clip("b.mp4", ClipKind::Audio, 0, 3.0, 2.0, 0.0);
    assert_eq!(tl.add_lane(ClipKind::Video, true), 0);
    assert_eq!(tl.lanes.video, 2);
    assert_eq!(
        (tl.clip(a).unwrap().track, tl.clip(b).unwrap().track),
        (1, 0)
    );
    // The new top lane is free for the clip that asked for it.
    assert!(tl.set_clip_track(a, 0));
    assert_eq!(tl.clip(a).unwrap().track, 0);
}

#[test]
fn a_clip_moves_only_to_a_lane_its_kind_has() {
    let mut tl = Timeline::empty_project();
    let a = tl.add_clip("a.mp4", ClipKind::Video, 0, 0.0, 2.0, 0.0);
    assert_eq!(tl.add_lane(ClipKind::Video, false), 1);
    assert!(tl.set_clip_track(a, 1));
    assert!(!tl.set_clip_track(a, 4), "no fifth video lane");
    assert_eq!(tl.clip(a).unwrap().track, 1);
    assert_eq!(tl.lanes.video, 2);
}

#[test]
fn a_version_1_timeline_sorts_its_shared_lanes_by_kind_and_keeps_their_order() {
    // Lanes as version 1 saved them: any kind on any lane, `track`
    // counting all of them. Lane 0 shows over lane 2 where they overlap.
    let mut tl = Timeline::new(30.0);
    let mut put = |name: &str, kind, track, start| {
        let id = tl.add_clip(name, kind, 0, start, 2.0, 0.0);
        tl.clip_mut(id).unwrap().track = track;
        id
    };
    let cutaway = put("cutaway", ClipKind::Video, 0, 1.0);
    let caption = put("caption", ClipKind::Title, 0, 4.0);
    let music = put("music", ClipKind::Audio, 1, 0.0);
    let base = put("base", ClipKind::Video, 2, 0.0);
    let lower = put("lower third", ClipKind::Title, 3, 1.0);
    tl.lanes = Lanes::default();
    tl.drop_empty_lanes();
    tl.ensure_lanes();
    let track = |id| tl.clip(id).unwrap().track;
    assert_eq!([track(cutaway), track(base)], [0, 1]);
    assert_eq!([track(caption), track(lower)], [0, 1]);
    assert_eq!(track(music), 0);
    assert_eq!(
        tl.lanes,
        Lanes {
            titles: 2,
            video: 2,
            audio: 1
        }
    );
    assert_eq!(tl.active_video(1.5).map(|clip| clip.id), Some(cutaway));
    assert_eq!(tl.lane_overlaps(), vec![]);
}

#[test]
fn split_preserves_source_and_levels() {
    let mut tl = Timeline::new(30.0);
    let track = 0;
    let id = tl.add_media_clip(
        "song.mp3",
        ClipKind::Audio,
        track,
        0.0,
        10.0,
        2.0,
        "/media/song.mp3",
    );
    tl.clip_mut(id).unwrap().volume = 0.4;
    let right = tl.split_at(id, 4.0).expect("split inside clip");
    let right = tl.clip(right).unwrap();
    assert_eq!(right.source.as_deref(), Some("/media/song.mp3"));
    assert_eq!(right.source_in, 6.0);
    assert!((right.volume - 0.4).abs() < f32::EPSILON);
}

#[test]
fn detach_audio_creates_linked_clip_and_silences_picture() {
    let mut tl = Timeline::empty_project();
    let video = tl.add_media_clip(
        "interview.mp4",
        ClipKind::Video,
        0,
        2.0,
        8.0,
        3.0,
        "/media/interview.mp4",
    );
    {
        let clip = tl.clip_mut(video).unwrap();
        clip.volume = 0.6;
        (clip.fade_in, clip.fade_out) = (0.5, 1.5);
        (clip.smooth_fades, clip.clean_voice, clip.duck) = (true, true, true);
    }

    let audio = tl.detach_audio(video).expect("detach embedded audio");
    let picture = tl.clip(video).unwrap().clone();
    let sound = tl.clip(audio).unwrap().clone();
    assert!(!picture.embedded_audio);
    // The sound keeps its fades and processing; the silent picture has none.
    assert_eq!((sound.fade_in, sound.fade_out), (0.5, 1.5));
    assert!(sound.smooth_fades && sound.clean_voice && sound.duck);
    assert_eq!((picture.fade_in, picture.fade_out), (0.0, 0.0));
    assert!(!picture.smooth_fades && !picture.clean_voice && !picture.duck);
    assert_eq!(picture.linked_clip, Some(audio));
    assert_eq!(sound.kind, ClipKind::Audio);
    assert_eq!(sound.linked_clip, Some(video));
    assert_eq!(sound.start, picture.start);
    assert_eq!(sound.duration, picture.duration);
    assert_eq!(sound.source_in, picture.source_in);
    assert_eq!(sound.source, picture.source);
    assert!((sound.volume - 0.6).abs() < f32::EPSILON);
    assert!(!sound.same_lane(&picture));

    tl.move_clip(video, 5.0);
    assert_eq!(tl.clip(audio).unwrap().start, 5.0, "linked audio moves");
    tl.trim_left(audio, 6.0);
    assert_eq!(tl.clip(video).unwrap().start, 6.0, "linked picture trims");
    assert_eq!(tl.clip(video).unwrap().source_in, 4.0);

    let right_video = tl.split_at(video, 8.0).expect("split linked pair");
    let right_audio_id = tl
        .clip(right_video)
        .unwrap()
        .linked_clip
        .expect("right picture stays linked");
    assert_ne!(right_audio_id, audio);
    assert_eq!(tl.clip(right_audio_id).unwrap().start, 8.0);
    assert_eq!(
        tl.clip(right_audio_id).unwrap().linked_clip,
        Some(right_video)
    );

    tl.remove_clip(right_audio_id);
    assert_eq!(tl.clip(right_video).unwrap().linked_clip, None);
    assert!(tl.set_embedded_audio(right_video, true));
    assert!(tl.clip(right_video).unwrap().embedded_audio);
}

#[test]
fn title_style_defaults_parses_and_survives_split() {
    let mut tl = Timeline::empty_project();
    let title = tl.add_clip("Hello", ClipKind::Title, 0, 0.0, 4.0, 0.0);
    assert_eq!(tl.clip(title).unwrap().title_style, TitleStyle::LowerThird);
    tl.clip_mut(title).unwrap().title_style =
        TitleStyle::from_name("pop-caption").expect("known style");
    let right = tl.split_at(title, 2.0).expect("split title");
    assert_eq!(tl.clip(right).unwrap().title_style, TitleStyle::PopCaption);
    assert_eq!(TitleStyle::Caption.as_str(), "caption");
    assert_eq!(
        TitleStyle::from_name("minimal-caption"),
        Some(TitleStyle::MinimalCaption)
    );
    assert_eq!(TitleStyle::from_name("comic-sans"), None);
}

#[test]
fn unlinking_detached_audio_keeps_both_clips_independent() {
    let mut tl = Timeline::empty_project();
    let video = tl.add_media_clip(
        "interview.mp4",
        ClipKind::Video,
        0,
        0.0,
        6.0,
        0.0,
        "/media/interview.mp4",
    );
    let audio = tl.detach_audio(video).expect("detach embedded audio");
    assert!(tl.unlink_clip(video));
    assert_eq!(tl.clip(video).unwrap().linked_clip, None);
    assert_eq!(tl.clip(audio).unwrap().linked_clip, None);
    tl.move_clip(video, 3.0);
    assert_eq!(tl.clip(audio).unwrap().start, 0.0);
}

#[test]
fn inserting_time_splits_crossing_clips_and_moves_everything_after() {
    let mut tl = Timeline::new(30.0);
    let early = tl.add_clip("Hello", ClipKind::Title, 2, 1.0, 1.0, 0.0);
    let talk = tl.add_media_clip("talk", ClipKind::Video, 1, 0.0, 10.0, 0.0, "/m/talk.mp4");
    let sound = tl.detach_audio(talk).expect("detach");
    let caption = tl.add_clip("across the point", ClipKind::Title, 2, 3.0, 3.0, 0.0);
    let broll = tl.add_media_clip("broll", ClipKind::Video, 0, 7.0, 2.0, 4.0, "/m/b.mp4");

    tl.insert_time(5.0, 4.0);

    let span = |id: ClipId| {
        let clip = tl.clip(id).unwrap();
        (clip.start, clip.end(), clip.source_in)
    };
    assert_eq!(span(early), (1.0, 2.0, 0.0), "clips before the point stay");
    assert_eq!(span(talk), (0.0, 5.0, 0.0));
    assert_eq!(span(sound), (0.0, 5.0, 0.0));
    assert_eq!(span(caption).0, 3.0);
    assert_eq!(span(caption).1, 5.0);
    assert_eq!(
        span(broll),
        (11.0, 13.0, 4.0),
        "later clips move by the gap"
    );
    let pieces = |name: &str| {
        let mut found: Vec<_> = tl
            .clips
            .iter()
            .filter(|c| c.name == name)
            .map(|c| (c.start, c.end(), c.source_in, c.linked_clip.is_some()))
            .collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found
    };
    assert_eq!(
        pieces("talk")[1],
        (9.0, 14.0, 5.0, true),
        "right piece resumes the source"
    );
    assert_eq!(
        pieces("across the point")[1].0,
        9.0,
        "titles keep their text after the gap"
    );
    let right_talk = tl
        .clips
        .iter()
        .find(|c| c.name == "talk" && c.start == 9.0)
        .unwrap();
    let partner = tl.clip(right_talk.linked_clip.unwrap()).unwrap();
    assert_eq!((partner.kind, partner.start), (ClipKind::Audio, 9.0));
    assert_eq!(tl.end(), 14.0);
    assert!(
        tl.clips.iter().all(|c| c.end() <= 5.0 || c.start >= 9.0),
        "nothing plays inside the inserted time"
    );
}

#[test]
fn ducked_sound_dips_under_other_clips_speech_and_recovers_in_pauses() {
    let mut tl = Timeline::new(30.0);
    let music = tl.add_media_clip("bed", ClipKind::Audio, 2, 2.0, 20.0, 0.0, "/m/bed.mp3");
    tl.clip_mut(music).unwrap().duck = true;
    let other_bed = tl.add_media_clip("bed2", ClipKind::Audio, 3, 0.0, 30.0, 0.0, "/m/bed2.mp3");
    tl.clip_mut(other_bed).unwrap().duck = true;
    tl.add_media_clip("talk", ClipKind::Video, 0, 5.0, 10.0, 10.0, "/m/talk.mp4");
    let muted = tl.add_media_clip("muted", ClipKind::Audio, 4, 0.0, 30.0, 0.0, "/m/muted.wav");
    tl.clip_mut(muted).unwrap().volume = 0.0;
    let speech: std::collections::HashMap<String, Vec<(f64, f64)>> = [
        // Source time; the talk clip shows source 10..20 at timeline 5..15.
        (
            "/m/talk.mp4".to_owned(),
            vec![(11.0, 13.0), (14.0, 14.5), (18.0, 30.0)],
        ),
        ("/m/bed2.mp3".to_owned(), vec![(0.0, 30.0)]),
        ("/m/muted.wav".to_owned(), vec![(0.0, 30.0)]),
    ]
    .into();
    let bed = tl.clip(music).unwrap().clone();
    // Timeline 6..8 and 9..9.5 (a 1 s pause stays down), then 13..15,
    // in the bed's own time (it starts at 2).
    assert_eq!(
        tl.duck_ranges(&bed, &speech),
        vec![(4.0, 7.5), (11.0, 13.0)]
    );
    let mut plain = bed.clone();
    plain.duck = false;
    assert!(tl.duck_ranges(&plain, &speech).is_empty());

    let ranges = tl.duck_ranges(&bed, &speech);
    let gain = |t: f64| duck_gain(t, &ranges);
    let near = |t: f64, want: f32| {
        assert!((gain(t) - want).abs() < 1e-6, "{t}: {} vs {want}", gain(t));
    };
    near(2.0, 1.0);
    // Raised-cosine in decibels: halfway is half the dip (-6 dB).
    near(3.9, 0.5);
    near(4.0, DUCK_LEVEL);
    near(7.7, DUCK_LEVEL);
    near(8.2, 0.5);
    near(8.6, 1.0);
    near(9.5, 1.0);
    near(12.0, DUCK_LEVEL);
}

#[test]
fn splitting_ducked_music_does_not_change_how_it_ducks() {
    // Speech at timeline 2..4 and 5..6 (a 1 s pause that stays down)
    // over a 12 s music bed.
    let mut tl = Timeline::new(30.0);
    let music = tl.add_media_clip("bed", ClipKind::Audio, 1, 0.0, 12.0, 0.0, "/m/bed.mp3");
    tl.clip_mut(music).unwrap().duck = true;
    tl.add_media_clip("talk", ClipKind::Video, 0, 0.0, 12.0, 0.0, "/m/talk.mp4");
    let speech: std::collections::HashMap<String, Vec<(f64, f64)>> =
        [("/m/talk.mp4".to_owned(), vec![(2.0, 4.0), (5.0, 6.0)])].into();
    let whole = tl.clip(music).unwrap().clone();
    let whole = tl.duck_ranges(&whole, &speech);
    let times = || (0..120).map(|step| f64::from(step) * 0.1 + 0.05);
    let unsplit: Vec<f32> = times().map(|t| duck_gain(t, &whole)).collect();
    // Cuts on the way down (1.9), in the pause (4.5), in the hold after
    // speech (6.1) and on the way back up (6.7): each piece must carry
    // on the curve of the speech just outside it.
    for cut in [1.9, 4.5, 6.1, 6.7] {
        let mut split = tl.clone();
        let right = split.split_at(music, cut).expect("split");
        let pieces = [music, right].map(|id| {
            let clip = split.clip(id).unwrap().clone();
            (clip.start, clip.end(), split.duck_ranges(&clip, &speech))
        });
        for (t, want) in times().zip(&unsplit) {
            let (start, _, ranges) = pieces
                .iter()
                .find(|(start, end, _)| (*start..*end).contains(&t))
                .unwrap();
            let got = duck_gain(t - start, ranges);
            assert!(
                (got - want).abs() < 1e-6,
                "cut {cut}, {t:.2} s: {got} vs {want}"
            );
        }
    }
    // Speech far from a clip leaves it alone.
    let late = tl.add_media_clip("late", ClipKind::Audio, 2, 7.2, 3.0, 0.0, "/m/bed.mp3");
    tl.clip_mut(late).unwrap().duck = true;
    let late = tl.clip(late).unwrap().clone();
    assert!(tl.duck_ranges(&late, &speech).is_empty());
}

#[test]
fn speed_keeps_the_source_range_through_split_trim_and_its_partner() {
    let mut tl = Timeline::new(30.0);
    let id = tl.add_media_clip(
        "talk.mp4",
        ClipKind::Video,
        0,
        1.0,
        4.0,
        2.0,
        "/media/talk.mp4",
    );
    {
        let clip = tl.clip_mut(id).unwrap();
        clip.transform.scale = 1.5;
        clip.color.contrast = 0.3;
        clip.background = Background::Blur;
    }
    let audio = tl.detach_audio(id).expect("detach");

    // Twice as fast: the same source seconds 2-6 in half the time, for both
    // of the pair.
    assert!(tl.set_speed(id, 2.0));
    for clip in [id, audio] {
        let clip = tl.clip(clip).unwrap();
        assert_eq!(
            (clip.duration, clip.speed, clip.source_end()),
            (2.0, 2.0, 6.0)
        );
    }
    let clip = tl.clip(id).unwrap();
    assert_eq!(clip.source_at(2.0), 4.0);
    assert_eq!(clip.time_at(5.0), 2.5);

    // Split one second in: source 2-4 stays left, 4-6 goes right, and the
    // right piece keeps the speed and the picture's look.
    let right = tl.split_at(id, 2.0).expect("split");
    assert_eq!(tl.clip(id).unwrap().source_end(), 4.0);
    let piece = tl.clip(right).unwrap();
    assert_eq!(
        (piece.start, piece.duration, piece.source_in, piece.speed),
        (2.0, 1.0, 4.0, 2.0)
    );
    assert_eq!(piece.transform.scale, 1.5);
    assert_eq!(piece.color.contrast, 0.3);
    assert_eq!(piece.background, Background::Blur);
    let partner = tl
        .clip(piece.linked_clip.expect("pieces stay linked"))
        .unwrap();
    assert_eq!((partner.source_in, partner.speed), (4.0, 2.0));

    // Half a second off the front is a second of source.
    tl.trim_left(right, 2.5);
    assert_eq!(tl.clip(right).unwrap().source_in, 5.0);

    // Slowing back down keeps the range: source 2-4 now takes 4 s.
    assert!(tl.set_speed(id, 0.5));
    let clip = tl.clip(id).unwrap();
    assert_eq!((clip.duration, clip.source_end()), (4.0, 4.0));

    // Out of range, titles and clips without a source are left alone.
    assert!(!tl.set_speed(id, 4.5));
    assert!(!tl.set_speed(id, 0.2));
    let title = tl.add_clip("Hi", ClipKind::Title, 0, 0.0, 1.0, 0.0);
    assert!(!tl.set_speed(title, 2.0));
    assert_eq!(tl.clip(id).unwrap().speed, 0.5);
    assert!(tl.in_supported_range());
    tl.clip_mut(id).unwrap().speed = 8.0;
    assert!(!tl.in_supported_range());
}

#[test]
fn room_after_start_stops_at_the_next_clip_on_either_lane_of_a_pair() {
    let mut tl = Timeline::new(30.0);
    let id = tl.add_media_clip("a.mp4", ClipKind::Video, 0, 1.0, 2.0, 0.0, "/media/a.mp4");
    tl.add_media_clip("b.mp4", ClipKind::Video, 0, 6.0, 2.0, 0.0, "/media/b.mp4");
    assert_eq!(tl.room_after_start(id), 5.0);
    let audio = tl.detach_audio(id).expect("detach");
    let lane = tl.clip(audio).unwrap().track;
    tl.add_media_clip(
        "c.wav",
        ClipKind::Audio,
        lane,
        4.0,
        1.0,
        0.0,
        "/media/c.wav",
    );
    assert_eq!(tl.room_after_start(id), 3.0);
    assert_eq!(tl.room_after_start(audio), 3.0);
}

#[test]
fn copies_keep_their_pair_linked_and_take_a_free_lane() {
    let mut tl = Timeline::empty_project();
    let v = tl.add_media_clip("v", ClipKind::Video, 0, 1.0, 2.0, 5.0, "/m/a.mp4");
    let a = tl.detach_audio(v).unwrap();
    tl.add_media_clip("blocker", ClipKind::Video, 0, 4.0, 2.0, 0.0, "/m/b.mp4");
    let pair = [tl.clip(v).unwrap().clone(), tl.clip(a).unwrap().clone()];

    let ids = tl.place_copies(&pair, 3.5);
    let (picture, sound) = (tl.clip(ids[0]).unwrap(), tl.clip(ids[1]).unwrap());
    // Video lane 0 is taken from 4 s, so the picture goes to a new lane;
    // sound lane 0 is free from 3.5 s, so the sound keeps its lane.
    assert_eq!(
        (
            picture.start,
            picture.track,
            picture.duration,
            picture.source_in
        ),
        (3.5, 1, 2.0, 5.0)
    );
    assert_eq!((sound.start, sound.track), (3.5, 0));
    assert!(
        !picture.embedded_audio,
        "the copy is still a detached picture"
    );
    assert_eq!(
        (picture.linked_clip, sound.linked_clip),
        (Some(ids[1]), Some(ids[0]))
    );
    assert_eq!(tl.clip(v).unwrap().linked_clip, Some(a));
    tl.move_clip(ids[0], 7.0);
    assert_eq!(
        tl.clip(ids[1]).unwrap().start,
        7.0,
        "the copies move together"
    );

    // The first lands at `start` and the rest keep their offset, but none
    // goes before zero.
    let mut early = pair.clone();
    early[1].start = 0.5;
    let ids = tl.place_copies(&early, 0.2);
    let starts: Vec<f64> = ids.iter().map(|id| tl.clip(*id).unwrap().start).collect();
    assert_eq!(starts, [0.5, 0.0]);
    assert!(tl.lane_overlaps().is_empty());
}

#[test]
fn ripple_delete_closes_the_gap_on_every_lane_that_has_room() {
    let mut tl = Timeline::empty_project();
    let media = |tl: &mut Timeline, kind, track, start: f64, end: f64| {
        tl.add_media_clip("m", kind, track, start, end - start, 0.0, "/m/a.mp4")
    };
    let v1 = media(&mut tl, ClipKind::Video, 0, 0.0, 2.0);
    let v2 = media(&mut tl, ClipKind::Video, 0, 2.0, 5.0);
    let v3 = media(&mut tl, ClipKind::Video, 0, 5.0, 7.0);
    let music = media(&mut tl, ClipKind::Audio, 0, 0.0, 10.0);
    let next_song = media(&mut tl, ClipKind::Audio, 0, 10.0, 12.0);
    let effect = media(&mut tl, ClipKind::Audio, 1, 6.0, 6.5);
    let v2_sound = tl.detach_audio(v2).unwrap();
    let over_v2 = tl.add_clip("said in v2", ClipKind::Title, 0, 2.5, 2.0, 0.0);
    let over_v3 = tl.add_clip("said in v3", ClipKind::Title, 0, 5.5, 1.0, 0.0);
    let into_v2 = tl.add_clip("from v1 into v2", ClipKind::Title, 0, 1.0, 1.4, 0.0);
    let out_of_v2 = tl.add_clip("from v2 into v3", ClipKind::Title, 0, 4.6, 0.8, 0.0);
    assert_eq!(
        tl.clip(v2_sound).unwrap().track,
        1,
        "under v2, beside the music"
    );

    let ripple = tl.ripple_delete(v2).unwrap();
    // v2's sound and the title over it go, and titles across its edges
    // lose the part over it; the music runs across the gap, so its lane,
    // and the song after it, stay where they were.
    assert_eq!(
        ripple,
        Ripple {
            titles: 1,
            lanes_kept: 1
        }
    );
    for gone in [v2, v2_sound, over_v2] {
        assert!(tl.clip(gone).is_none());
    }
    let start = |tl: &Timeline, id| tl.clip(id).unwrap().start;
    assert_eq!(
        [v1, v3, over_v3, effect, music, next_song].map(|id| start(&tl, id)),
        [0.0, 2.0, 2.5, 3.0, 0.0, 10.0]
    );
    let span = |tl: &Timeline, id| {
        let c = tl.clip(id).unwrap();
        [c.start, c.duration, c.source_in].map(|t| (t * 1000.0).round() / 1000.0)
    };
    assert_eq!(span(&tl, into_v2), [1.0, 1.0, 0.0]);
    assert_eq!(
        span(&tl, out_of_v2),
        [2.0, 0.4, 0.4],
        "its words keep their times"
    );
    assert!(tl.lane_overlaps().is_empty());

    // Over a cutaway the base still shows, so its titles stay.
    let mut tl = Timeline::empty_project();
    let base = media(&mut tl, ClipKind::Video, 1, 0.0, 10.0);
    let cutaway = media(&mut tl, ClipKind::Video, 0, 2.0, 4.0);
    let line = tl.add_clip("said over the base", ClipKind::Title, 0, 2.5, 1.0, 0.0);
    let ripple = tl.ripple_delete(cutaway).unwrap();
    assert_eq!(
        ripple,
        Ripple {
            titles: 0,
            lanes_kept: 0
        }
    );
    assert_eq!((start(&tl, base), start(&tl, line)), (0.0, 2.5));
}

#[test]
fn a_freeze_frame_holds_the_picture_at_the_playhead_and_moves_later_clips() {
    let mut tl = Timeline::empty_project();
    let v = tl.add_media_clip("v", ClipKind::Video, 0, 1.0, 4.0, 10.0, "/m/a.mp4");
    assert!(tl.set_speed(v, 2.0));
    let v_sound = tl.detach_audio(v).unwrap();
    let after = tl.add_media_clip("after", ClipKind::Video, 0, 3.0, 1.0, 0.0, "/m/b.mp4");
    let music = tl.add_media_clip("music", ClipKind::Audio, 1, 0.0, 8.0, 0.0, "/m/c.mp3");
    let across = tl.add_clip("across", ClipKind::Title, 0, 1.5, 1.0, 0.0);
    let later = tl.add_clip("later", ClipKind::Title, 0, 3.2, 0.4, 0.0);

    // 0.4 of a frame past 2 s: the frame on screen started at 2 s, when the
    // 2x clip shows source second 10 + (2 - 1) * 2 = 12.
    let freeze = tl.freeze_frame(v, 2.0 + 0.4 / 30.0, 3.0).unwrap();
    let f = tl.clip(freeze).unwrap().clone();
    assert!(f.freeze && f.still() && !f.embedded_audio && f.linked_clip.is_none());
    assert_eq!(
        (f.start, f.duration, f.source_in, f.speed, f.track),
        (2.0, 3.0, 12.0, 1.0, 0)
    );
    assert_eq!(f.source_at(4.9), 12.0);
    let clip = |tl: &Timeline, id| tl.clip(id).unwrap().clone();
    assert_eq!((clip(&tl, v).start, clip(&tl, v).end()), (1.0, 2.0));
    let right = tl
        .clips
        .iter()
        .find(|c| c.name == "v" && c.start == 5.0)
        .unwrap();
    assert_eq!(
        (right.source_in, right.speed, right.end()),
        (12.0, 2.0, 6.0)
    );
    let right_sound = tl.clip(right.linked_clip.unwrap()).unwrap();
    assert_eq!((right_sound.start, right_sound.source_in), (5.0, 12.0));
    assert_eq!(clip(&tl, v_sound).end(), 2.0);
    // Later clips move on every lane; those running across stay whole.
    assert_eq!(
        [after, later, music, across].map(|id| (clip(&tl, id).start, clip(&tl, id).duration)),
        [(6.0, 1.0), (6.2, 0.4), (0.0, 8.0), (1.5, 1.0)]
    );
    assert!(tl.lane_overlaps().is_empty());

    // A still splits into the same picture, and does not freeze again.
    let piece = tl.split_at(freeze, 3.0).unwrap();
    assert_eq!(clip(&tl, piece).source_in, 12.0);
    assert_eq!(tl.freeze_frame(freeze, 2.5, 1.0), None);
    let title = tl.add_clip("t", ClipKind::Title, 1, 0.0, 1.0, 0.0);
    assert_eq!(tl.freeze_frame(title, 0.5, 1.0), None);
    assert_eq!(tl.freeze_frame(after, 9.0, 1.0), None, "outside the clip");

    // At a clip's first frame nothing splits: the hold goes before it.
    let before = tl.clips.len();
    let first = tl.freeze_frame(after, 6.0, 1.0).unwrap();
    assert_eq!(tl.clips.len(), before + 1);
    assert_eq!(
        (clip(&tl, first).start, clip(&tl, first).source_in),
        (6.0, 0.0)
    );
    assert_eq!(clip(&tl, after).start, 7.0);
}

#[test]
fn a_transition_sits_on_the_cut_into_its_clip_and_takes_at_most_half_of_each_side() {
    let mut tl = Timeline::new(30.0);
    let a = tl.add_media_clip("a", ClipKind::Video, 1, 0.0, 4.0, 1.0, "/m/a.mp4");
    // A media edge a third of a frame off the cut still meets it.
    let b = tl.add_media_clip("b", ClipKind::Video, 1, 4.01, 2.0, 0.0, "/m/b.mp4");
    tl.clip_mut(b).unwrap().transition = Some(Transition::new(TransitionKind::Mix, 1.0));
    assert_eq!(tl.clip_before(b).map(|clip| clip.id), Some(a));
    assert_eq!(tl.clip_before(a), None);
    let span = tl.transitions()[0];
    assert_eq!((span.from, span.to, span.kind), (a, b, TransitionKind::Mix));
    assert_eq!((span.start, span.cut, span.end), (3.51, 4.01, 4.51));
    assert!((span.progress(3.76) - 0.25).abs() < 1e-9);

    // Past the cut the program still plays the clip before, to the end.
    let program = |tl: &Timeline, at: f64| tl.program_video(at).map(|clip| clip.id);
    assert_eq!(tl.active_video(4.2).map(|clip| clip.id), Some(b));
    assert_eq!(program(&tl, 4.2), Some(a));
    assert_eq!(program(&tl, 3.4), Some(a));
    assert_eq!(
        program(&tl, 4.51),
        Some(b),
        "the end is the next clip's own"
    );

    // A lower lane covering part of it hides that part.
    let cover = tl.add_media_clip("c", ClipKind::Video, 0, 3.6, 0.3, 0.0, "/m/c.mp4");
    assert_eq!(tl.transition_at(3.7), None);
    assert_eq!(program(&tl, 3.7), Some(cover));
    assert_eq!(tl.transition_at(3.55).map(|span| span.to), Some(b));
    tl.remove_clip(cover);

    // Each side takes at most half its clip.
    tl.trim_right(b, 4.6);
    let half = tl.clip(b).unwrap().duration / 2.0;
    assert!(half < 0.5);
    let span = tl.transitions()[0];
    assert!((span.end - span.cut - half).abs() < 1e-9, "{span:?}");
    assert!((span.cut - span.start - half).abs() < 1e-9, "{span:?}");

    // The cut a split leaves has none, and a copy brings none along.
    tl.trim_right(b, 6.01);
    let right = tl.split_at(b, 5.0).unwrap();
    assert!(tl.clip(b).unwrap().transition.is_some());
    assert_eq!(tl.clip(right).unwrap().transition, None);
    let copied = tl.clip(b).unwrap().clone();
    let copy = tl.place_copies(&[copied], 10.0)[0];
    assert_eq!(tl.clip(copy).unwrap().transition, None);
    assert_eq!(tl.transitions().len(), 1);

    // Moved away from its cut it does nothing; it is kept for when a clip
    // ends there again.
    tl.move_clip(b, 20.0);
    assert!(tl.transitions().is_empty());
    assert_eq!(program(&tl, 3.9), Some(a));

    // Out of range lengths are not a project the editor opens.
    tl.clip_mut(b).unwrap().transition = Some(Transition {
        kind: TransitionKind::Mix,
        duration: f64::NAN,
    });
    assert!(!tl.in_supported_range());
    assert_eq!(
        Transition::new(TransitionKind::Mix, 9.0).duration,
        MAX_TRANSITION
    );
    assert_eq!(
        TransitionKind::from_name("Dip to white"),
        Some(TransitionKind::DipWhite)
    );
    assert_eq!(
        TransitionKind::from_name("slide-up"),
        Some(TransitionKind::SlideUp)
    );
}

/// The monitor draws what `xfade` computes: from progress `p = 1 - t`, a
/// wipe's edge at `int(w * p)`, a slide by `int(w * p)`.
#[test]
fn a_transition_frame_follows_xfade_arithmetic() {
    let canvas = (100, 50);
    let at = |kind: TransitionKind, t| kind.frame(t, canvas);
    let mix = at(TransitionKind::Mix, 0.25);
    assert_eq!(
        (mix.from.opacity, mix.to.opacity, mix.veil),
        (1.0, 0.25, None)
    );
    // 75 pixels of the old picture stay, then the edge pixel, column 75.
    assert_eq!(
        at(TransitionKind::WipeLeft, 0.25).to.window,
        [76.0, 0.0, 24.0, 50.0]
    );
    assert_eq!(
        at(TransitionKind::WipeRight, 0.25).to.window,
        [0.0, 0.0, 26.0, 50.0]
    );
    assert_eq!(
        at(TransitionKind::WipeUp, 0.5).to.window,
        [0.0, 26.0, 100.0, 24.0]
    );
    assert_eq!(
        at(TransitionKind::WipeDown, 0.5).to.window,
        [0.0, 0.0, 100.0, 26.0]
    );
    assert_eq!(at(TransitionKind::WipeLeft, 0.0).to.window[2], 0.0);
    let slide = at(TransitionKind::SlideLeft, 0.25);
    assert_eq!(
        (slide.from.offset, slide.to.offset),
        ([-25.0, 0.0], [75.0, 0.0])
    );
    let slide = at(TransitionKind::SlideRight, 0.25);
    assert_eq!(
        (slide.from.offset, slide.to.offset),
        ([25.0, 0.0], [-75.0, 0.0])
    );
    let slide = at(TransitionKind::SlideDown, 0.5);
    assert_eq!(
        (slide.from.offset, slide.to.offset),
        ([0.0, 25.0], [0.0, -25.0])
    );
    let dip = at(TransitionKind::DipWhite, 0.25);
    assert_eq!(
        (dip.from.opacity, dip.to.opacity, dip.veil),
        (1.0, 0.0, Some((true, 0.5)))
    );
    let dip = at(TransitionKind::DipBlack, 0.75);
    assert_eq!(
        (dip.from.opacity, dip.to.opacity, dip.veil),
        (0.0, 1.0, Some((false, 0.5)))
    );
}

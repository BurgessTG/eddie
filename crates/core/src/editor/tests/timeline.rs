//! Editing clips on the timeline and adding media to it.

use super::*;

/// Free-form timeline: deleting a clip leaves its gap, magnet
/// on included. The magnet only snaps edges while dragging.
#[test]
fn delete_with_magnet_keeps_gap() {
    let mut app = Editor::headless();
    assert!(app.viewport.magnet);
    app.timeline
        .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 0.0);
    let b_id = app
        .timeline
        .add_clip("b.mp4", ClipKind::Video, 0, 10.0, 10.0, 0.0);
    let c_id = app
        .timeline
        .add_clip("c.mp4", ClipKind::Video, 0, 20.0, 10.0, 0.0);
    app.selected = Some(b_id);
    app.perform(Action::DeleteSelected);
    assert!(app.timeline.clip(b_id).is_none());
    let c = app.timeline.clip(c_id).unwrap();
    assert!(
        (c.start - 20.0).abs() < 1e-6,
        "magnet delete should leave the gap, got {}",
        c.start
    );
}

#[test]
fn delete_without_magnet_keeps_gap() {
    let mut app = Editor::headless();
    app.viewport.magnet = false;
    app.timeline
        .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 0.0);
    let b_id = app
        .timeline
        .add_clip("b.mp4", ClipKind::Video, 0, 10.0, 10.0, 0.0);
    let c_id = app
        .timeline
        .add_clip("c.mp4", ClipKind::Video, 0, 20.0, 10.0, 0.0);
    app.selected = Some(b_id);
    app.perform(Action::DeleteSelected);
    let c = app.timeline.clip(c_id).unwrap();
    assert!(
        (c.start - 20.0).abs() < 1e-6,
        "magnet off should leave the gap, got {}",
        c.start
    );
}

/// Split acts on the playhead, not on a click point: with no
/// selection it cuts the clip under the playhead.
#[test]
fn split_at_playhead_cuts_clip_under_playhead_without_selection() {
    let mut app = Editor::headless();
    let clip_id = app
        .timeline
        .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 2.0);
    app.selected = None;
    app.playhead = 4.0;
    app.perform(Action::SplitSelected);
    assert_eq!(app.timeline.clips.len(), 2);
    let left = app.timeline.clip(clip_id).unwrap();
    assert!((left.end() - 4.0).abs() < 0.02, "left end {}", left.end());
    let right_id = app.selected.expect("right half selected");
    let right = app.timeline.clip(right_id).unwrap();
    assert!((right.start - 4.0).abs() < 0.02);
    assert!((right.source_in - 6.0).abs() < 0.02);
}

#[test]
fn video_of_the_other_orientation_is_never_added_to_the_project() {
    let mut app = Editor::headless();
    // Displayed sizes: a rotated phone clip probes as portrait.
    for (name, width, height) in [("wide.mp4", 1920, 1080), ("phone.mov", 1080, 1920)] {
        let path = format!("/media/{name}");
        let scene = sweep::SweepCandidate {
            id: format!("{path}#scene-0"),
            asset_path: path.clone(),
            asset_name: name.to_owned(),
            title: name.to_owned(),
            source_in: 1.0,
            duration: 2.0,
            score: 80.0,
            reason: "Detected shot.".to_owned(),
            stability: 0.9,
            audio_energy: 0.4,
            audio_peak: 0.7,
            category: "broll".to_owned(),
            tags: Vec::new(),
            motion_score: 0.3,
            audio_score: 0.5,
            music_bed: false,
            visual: None,
            has_video: true,
            transcript: Vec::new(),
            review: sweep::ClipReview::default(),
            dismissed: false,
        };
        app.assets.push(BinAsset {
            info: MediaInfo {
                path,
                name: name.to_owned(),
                duration: 10.0,
                width,
                height,
                fps: 30.0,
                has_video: true,
                has_audio: true,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData {
                candidates: vec![scene.clone()],
                scene_clips: vec![scene],
                ..Default::default()
            },
        });
    }

    // Bulk adds skip the clashing source instead of leaving its slot.
    app.add_all_scene_clips();
    let sources: Vec<_> = app
        .timeline
        .clips
        .iter()
        .map(|c| c.source.clone())
        .collect();
    assert_eq!(sources, vec![Some("/media/wide.mp4".to_owned())]);
    assert_eq!(app.timeline.end(), 2.0);
    assert!(app.status.contains("skipped 1"), "{}", app.status);

    // Every single-add path, scripts included, shares the refusal.
    let refused = app.try_add_asset_range_to_timeline("/media/phone.mov", "", None, 5.0, 0.0, 1.0);
    assert_eq!(
        refused,
        Err("phone.mov is portrait; the project is landscape.".to_owned())
    );
    assert_eq!(app.timeline.clips.len(), 1);

    // A portrait project accepts it, and the canvas stays portrait.
    app.set_format(Orientation::Portrait);
    assert!(
        app.status.contains("1 clip doesn't match"),
        "{}",
        app.status
    );
    app.try_add_asset_range_to_timeline("/media/phone.mov", "", None, 5.0, 0.0, 1.0)
        .expect("portrait video fits a portrait project");
    app.perform(Action::ClearTimeline);
    assert_eq!(app.timeline.orientation, Orientation::Portrait);
}

#[test]
fn adding_all_scene_clips_reconstructs_the_source_in_order() {
    let mut app = Editor::headless();
    let scene = |index: usize, source_in: f64, duration: f64, score: f32| sweep::SweepCandidate {
        id: format!("/media/montage.mp4#scene-{index}"),
        asset_path: "/media/montage.mp4".to_owned(),
        asset_name: "montage.mp4".to_owned(),
        title: format!("montage.mp4 · scene {index}"),
        source_in,
        duration,
        score,
        reason: "Detected shot.".to_owned(),
        stability: 0.9,
        audio_energy: 0.4,
        audio_peak: 0.7,
        category: "broll".to_owned(),
        tags: Vec::new(),
        motion_score: 0.3,
        audio_score: 0.5,
        music_bed: false,
        visual: None,
        has_video: true,
        transcript: Vec::new(),
        review: sweep::ClipReview::default(),
        dismissed: false,
    };
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/montage.mp4".to_owned(),
            name: "montage.mp4".to_owned(),
            duration: 20.0,
            width: 854,
            height: 480,
            fps: 30.0,
            has_video: true,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData {
            candidates: vec![scene(99, 8.0, 2.0, 99.0)],
            scene_clips: vec![
                scene(2, 8.0, 7.0, 10.0),
                scene(0, 0.0, 3.0, 90.0),
                scene(3, 15.0, 5.0, 50.0),
                scene(1, 3.0, 5.0, 70.0),
            ],
            ..Default::default()
        },
    });

    app.add_all_scene_clips();
    let mut clips: Vec<_> = app.timeline.clips.iter().collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    assert_eq!(clips.len(), 4, "scene action ignores ranked picks");
    assert_eq!(
        clips.iter().map(|clip| clip.source_in).collect::<Vec<_>>(),
        vec![0.0, 3.0, 8.0, 15.0]
    );
    for pair in clips.windows(2) {
        assert_eq!(pair[0].end(), pair[1].start);
    }
    assert_eq!(app.timeline.end(), 20.0);

    for boundaries in [
        vec![
            0.0,
            1.0 / 3.0,
            49.0 / 60.0,
            1.235,
            1.585,
            2.135,
            3.2866666666666666,
        ],
        vec![0.0, 1.0 / 120.0, 1.0 / 60.0, 0.157, 0.158, 0.5],
    ] {
        app.assets[0].info.duration = *boundaries.last().unwrap();
        app.assets[0].sweep.scene_clips = boundaries
            .windows(2)
            .enumerate()
            .map(|(index, pair)| scene(index, pair[0], pair[1] - pair[0], 70.0))
            .rev()
            .collect();
        let count = boundaries.len() - 1;
        for route in ["all scenes", "individual", "console"] {
            app.timeline = Timeline::empty_project();
            for batch in 0..2 {
                let start = app.timeline.end();
                match route {
                    "all scenes" => app.add_all_scene_clips(),
                    "individual" => {
                        for index in 0..count {
                            app.add_sweep_candidate(&format!("/media/montage.mp4#scene-{index}"));
                        }
                    }
                    "console" => app.run_console(
                        "(() => { let at = clips().reduce((end, c) => Math.max(end, c.start + c.duration), 0); sceneClips().sort((a, b) => a.sourceIn - b.sourceIn).forEach(c => { addAssetRange(c.asset, at, c.sourceIn, c.duration); at += c.duration; }); })()".to_owned(),
                    ),
                    _ => unreachable!(),
                }
                assert_eq!(app.timeline.clips.len(), (batch + 1) * count, "{route}");
                assert_eq!(app.timeline.track_labels.len(), 1, "{route}: one free lane");
                let clips = &app.timeline.clips[batch * count..];
                for (clip, pair) in clips.iter().zip(boundaries.windows(2)) {
                    assert_eq!(clip.track, 0, "{route}");
                    assert!(
                        (clip.start - start - pair[0]).abs() < 1e-12,
                        "{route}: {clip:?}"
                    );
                    assert_eq!(clip.source_in, pair[0], "{route}");
                    assert_eq!(clip.duration, pair[1] - pair[0], "{route}");
                }
                for pair in app.timeline.clips.windows(2) {
                    assert_eq!(
                        pair[0].end(),
                        pair[1].start,
                        "{route}: contiguous placement"
                    );
                }
                assert!((app.timeline.end() - start - boundaries.last().unwrap()).abs() < 1e-12);
            }
        }
    }

    let placed = app.timeline.clips.clone();
    let gap = app
        .try_add_asset_range_to_timeline(
            "/media/montage.mp4",
            "after a gap",
            None,
            3.015,
            0.0,
            0.137,
        )
        .unwrap();
    let overlap = app
        .try_add_asset_range_to_timeline("/media/montage.mp4", "overlap", None, 3.04, 0.1, 0.01)
        .unwrap();
    let explicit = app
        .try_add_asset_range_to_timeline(
            "/media/montage.mp4",
            "explicit lane",
            Some(0),
            3.045,
            0.2,
            0.01,
        )
        .unwrap();
    let clamped = app
        .try_add_asset_range_to_timeline("/media/montage.mp4", "source end", None, 4.0, 0.49, 1.0)
        .unwrap();
    assert_eq!(
        &app.timeline.clips[..placed.len()],
        placed.as_slice(),
        "adds do not ripple existing clips"
    );
    assert_eq!(app.timeline.clip(gap).unwrap().start, 3.015);
    assert_eq!(app.timeline.clip(gap).unwrap().track, 0);
    assert_eq!(app.timeline.clip(overlap).unwrap().track, 1);
    assert_eq!(app.timeline.clip(explicit).unwrap().track, 0);
    assert!((app.timeline.clip(clamped).unwrap().duration - 0.01).abs() < 1e-12);
}

#[test]
fn adding_a_music_bed_range_starts_muted() {
    let mut app = Editor::headless();
    let candidate = |music_bed: bool| sweep::SweepCandidate {
        id: "/media/montage.mp4#0".to_owned(),
        asset_path: "/media/montage.mp4".to_owned(),
        asset_name: "montage.mp4".to_owned(),
        title: "montage.mp4 · 0:00".to_owned(),
        source_in: 0.0,
        duration: 20.0,
        score: 80.0,
        reason: "test".to_owned(),
        stability: 0.9,
        audio_energy: 0.6,
        audio_peak: 0.9,
        category: "broll".to_owned(),
        tags: Vec::new(),
        motion_score: 0.3,
        audio_score: 0.8,
        music_bed,
        visual: None,
        has_video: true,
        transcript: Vec::new(),
        review: sweep::ClipReview::default(),
        dismissed: false,
    };
    let asset = |music_bed: bool| BinAsset {
        info: MediaInfo {
            path: "/media/montage.mp4".to_owned(),
            name: "montage.mp4".to_owned(),
            duration: 60.0,
            width: 854,
            height: 480,
            fps: 30.0,
            has_video: true,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData {
            candidates: vec![candidate(music_bed)],
            ..Default::default()
        },
    };
    app.assets.push(asset(true));
    app.add_asset_range_to_timeline("/media/montage.mp4", "bed", None, 0.0, 2.0, 6.0);
    let muted = app.timeline.clips.last().expect("clip added");
    assert_eq!(muted.volume, 0.0, "music-bed range starts muted");

    app.mute_music_beds = false;
    app.add_asset_range_to_timeline("/media/montage.mp4", "bed again", None, 20.0, 2.0, 6.0);
    let audible = app.timeline.clips.last().expect("clip added");
    assert_eq!(
        audible.volume, 1.0,
        "the same music bed stays audible when the user opts out of muting"
    );

    app.mute_music_beds = true;
    app.assets[0].sweep.scene_clips = vec![candidate(true)];
    let mut speech = candidate(false);
    speech.source_in = 2.0;
    speech.duration = 6.0;
    speech.review.status = sweep::ReviewStatus::Reviewed;
    app.assets[0].sweep.candidates = vec![speech];
    app.add_asset_range_to_timeline(
        "/media/montage.mp4",
        "reviewed speech",
        None,
        30.0,
        2.0,
        6.0,
    );
    assert_eq!(
        app.timeline.clips.last().unwrap().volume,
        1.0,
        "range-specific speech review overrides a whole-scene music hint"
    );
    app.add_asset_range_to_timeline("/media/montage.mp4", "unreviewed bed", None, 40.0, 8.0, 6.0);
    assert_eq!(
        app.timeline.clips.last().unwrap().volume,
        0.0,
        "unreviewed portions retain the scene evidence"
    );

    // A curated audio-only track is the new soundtrack, not baked-in score.
    let mut song = asset(true);
    song.info.path = "/media/song.mp3".to_owned();
    song.info.name = "song.mp3".to_owned();
    song.info.has_video = false;
    song.sweep.candidates[0].has_video = false;
    song.sweep.candidates[0].asset_path = song.info.path.clone();
    app.assets.push(song);
    app.add_asset_range_to_timeline("/media/song.mp3", "song", None, 0.0, 0.0, 6.0);
    assert_eq!(
        app.timeline.clips.last().unwrap().volume,
        1.0,
        "an added music track is never muted as a music bed"
    );
}

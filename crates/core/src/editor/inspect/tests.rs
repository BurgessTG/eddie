use super::*;
use crate::editor::media::{AssetState, BinAsset};
use crate::editor::sweep::SweepData;

fn asset(path: &str, width: u32, height: u32) -> BinAsset {
    BinAsset {
        info: openreel_media::MediaInfo {
            path: path.to_owned(),
            name: path.to_owned(),
            duration: 10.0,
            width,
            height,
            fps: 30.0,
            has_video: true,
            has_audio: true,
            photo: false,
            alpha: false,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: SweepData::default(),
    }
}

fn kinds(findings: &[Finding]) -> Vec<(&'static str, Vec<ClipId>)> {
    findings
        .iter()
        .map(|finding| (finding.kind, finding.clips.clone()))
        .collect()
}

/// Each check fires on the clip that breaks it and stays quiet on a
/// clean neighbour, including a titled gap and a short final clip.
#[test]
fn inspect_reports_broken_and_suspicious_clips() {
    let mut timeline = Timeline::empty_project();
    let fps = timeline.fps;
    let a = timeline.add_media_clip("A", ClipKind::Video, 0, 0.0, 2.0, 0.0, "a.mp4");
    // A one-frame flash between two shots.
    let flash = timeline.add_media_clip("F", ClipKind::Video, 0, 2.0, 1.0 / fps, 3.0, "a.mp4");
    let b = timeline.add_media_clip("B", ClipKind::Video, 0, 2.0 + 1.0 / fps, 2.0, 5.0, "a.mp4");
    // Untitled black gap, then a titled one (a title card is deliberate).
    let end_b = 4.0 + 1.0 / fps;
    let c = timeline.add_media_clip("C", ClipKind::Video, 0, end_b + 1.0, 1.0, 9.5, "a.mp4");
    let card = timeline.add_clip("Day two", ClipKind::Title, 3, end_b + 2.0, 1.0, 0.0);
    // Moved across, the card stays on the canvas; a title moved down past
    // the bottom does not.
    timeline.clip_mut(card).unwrap().text.x = 0.6;
    let dropped = timeline.add_clip("Below", ClipKind::Title, 2, 0.0, 1.0, 0.0);
    timeline.clip_mut(dropped).unwrap().text.y = 0.2;
    // Portrait on a landscape canvas is fine; moved off it, it shows black.
    let d = timeline.add_media_clip("D", ClipKind::Video, 0, end_b + 3.0, 1.0, 0.0, "tall.mp4");
    timeline.clip_mut(d).unwrap().transform.x = -0.7;
    // Under clip A on a higher lane: never visible, and silent.
    let hidden = timeline.add_media_clip("H", ClipKind::Video, 1, 0.5, 1.0, 0.0, "a.mp4");
    timeline.clip_mut(hidden).unwrap().embedded_audio = false;
    // Also covered, but heard: possibly a voice under a cutaway.
    let voice = timeline.add_media_clip("V", ClipKind::Video, 4, 0.5, 1.0, 0.0, "a.mp4");
    let missing = timeline.add_media_clip("M", ClipKind::Audio, 2, 0.0, 1.0, 0.0, "gone.wav");
    // The last frame of the timeline may be short.
    let tail = timeline.add_media_clip(
        "T",
        ClipKind::Video,
        0,
        end_b + 4.0,
        0.5 / fps,
        1.0,
        "a.mp4",
    );
    timeline.clip_mut(b).unwrap().fade_in = 1.5;
    timeline.clip_mut(b).unwrap().fade_out = 1.0;
    let mut text = asset("a.mp4", 1920, 1080);
    text.sweep
        .candidates
        .push(crate::editor::sweep::SweepCandidate {
            id: "a.mp4#5.5".to_owned(),
            asset_path: "a.mp4".to_owned(),
            asset_name: "a.mp4".to_owned(),
            title: "a.mp4".to_owned(),
            source_in: 5.5,
            duration: 0.5,
            score: 90.0,
            reason: "test".to_owned(),
            stability: 0.9,
            audio_energy: 0.1,
            audio_peak: 0.1,
            category: "broll".to_owned(),
            tags: vec!["on-screen-text".to_owned()],
            motion_score: 0.3,
            audio_score: 0.1,
            music_bed: false,
            visual: None,
            has_video: true,
            transcript: Vec::new(),
            review: crate::editor::sweep::ClipReview::default(),
            dismissed: false,
        });
    let assets = [text, asset("tall.mp4", 1080, 1920)];
    let found = kinds(&inspect(&timeline, &assets));
    let expected = vec![
        ("fade-too-long", vec![b]),
        ("on-screen-text", vec![b]),
        ("source-range", vec![c]),
        ("off-canvas", vec![d]),
        ("off-canvas", vec![dropped]),
        ("missing-media", vec![missing]),
        ("flash-frame", vec![flash]),
        ("picture-gap", vec![]),
        ("hidden-clip", vec![hidden]),
        ("hidden-clip", vec![voice]),
    ];
    for item in &expected {
        assert!(found.contains(item), "missing {item:?} in {found:?}");
    }
    assert_eq!(
        found.len(),
        expected.len(),
        "unexpected findings: {found:?}"
    );
    let gap = inspect(&timeline, &assets)
        .into_iter()
        .find(|finding| finding.kind == "picture-gap")
        .unwrap();
    assert!((gap.start - end_b).abs() < 1e-6 && (gap.end - (end_b + 1.0)).abs() < 1e-6);
    let covered = |clip: ClipId| {
        inspect(&timeline, &assets)
            .into_iter()
            .find(|finding| finding.clips == [clip])
            .unwrap()
            .message
    };
    assert_eq!(
        covered(hidden),
        format!("Clip {hidden} on lane 1 is always covered by a lane above it: it never shows.")
    );
    assert_eq!(
        covered(voice),
        format!(
            "Clip {voice} on lane 4 is always covered by a lane above it: its picture never \
                 shows, only its sound plays."
        )
    );
    let _ = (a, card, tail);
}

/// Same-shot cuts a few frames off pop; exact continuations, real jump
/// cuts and cuts between sources do not.
#[test]
fn near_match_cuts_report_skipped_and_repeated_frames() {
    let mut timeline = Timeline::empty_project();
    let frame = 1.0 / 30.0;
    // a.mp4 runs at 30 fps: 2 frames skipped, then a clean continuation,
    // then 1 frame repeated, then a 10-frame jump cut, then another file.
    let a = timeline.add_media_clip("A", ClipKind::Video, 0, 0.0, 1.0, 0.0, "a.mp4");
    let b = timeline.add_media_clip(
        "B",
        ClipKind::Video,
        0,
        1.0,
        1.0,
        1.0 + 2.0 * frame,
        "a.mp4",
    );
    let c = timeline.add_media_clip(
        "C",
        ClipKind::Video,
        0,
        2.0,
        1.0,
        2.0 + 2.0 * frame,
        "a.mp4",
    );
    let d = timeline.add_media_clip("D", ClipKind::Video, 0, 3.0, 1.0, 3.0 + frame, "a.mp4");
    let e = timeline.add_media_clip(
        "E",
        ClipKind::Video,
        0,
        4.0,
        1.0,
        4.0 + 11.0 * frame,
        "a.mp4",
    );
    timeline.add_media_clip(
        "F",
        ClipKind::Video,
        0,
        5.0,
        1.0,
        5.0 + 11.0 * frame,
        "b.mp4",
    );
    let assets = [asset("a.mp4", 1920, 1080), asset("b.mp4", 1920, 1080)];
    let found = inspect(&timeline, &assets);
    let pops: Vec<_> = found
        .iter()
        .filter(|finding| finding.kind == "near-match-cut")
        .collect();
    assert_eq!(
        pops.iter().map(|f| f.clips.clone()).collect::<Vec<_>>(),
        vec![vec![a, b], vec![c, d]],
        "{found:?}"
    );
    assert!(pops[0].message.contains("skips 2"), "{}", pops[0].message);
    assert!(pops[1].message.contains("repeats 1"), "{}", pops[1].message);
    let _ = e;
}

/// A clip under a smaller picture shows around it, so it is not hidden.
#[test]
fn a_clip_under_a_picture_in_picture_is_not_hidden() {
    let mut timeline = Timeline::empty_project();
    let base = timeline.add_media_clip("B", ClipKind::Video, 1, 0.0, 4.0, 0.0, "a.mp4");
    let top = timeline.add_media_clip("T", ClipKind::Video, 0, 0.0, 4.0, 0.0, "a.mp4");
    let assets = [asset("a.mp4", 1920, 1080)];
    let hidden = |timeline: &Timeline| {
        kinds(&inspect(timeline, &assets))
            .into_iter()
            .filter(|(kind, _)| *kind == "hidden-clip")
            .collect::<Vec<_>>()
    };
    assert_eq!(hidden(&timeline), vec![("hidden-clip", vec![base])]);
    timeline.clip_mut(top).unwrap().transform.scale = 0.4;
    assert_eq!(hidden(&timeline), vec![]);
}

/// A blended or keyed picture mixes with the lanes under it: over one it
/// is not covering it, and with nothing under it the blend has only black
/// and the keyed screen shows black.
#[test]
fn a_blended_or_keyed_picture_shows_the_lane_under_it_and_is_reported_over_black() {
    let mut timeline = Timeline::empty_project();
    let base = timeline.add_media_clip("B", ClipKind::Video, 1, 0.0, 4.0, 0.0, "a.mp4");
    let top = timeline.add_media_clip("T", ClipKind::Video, 0, 0.0, 4.0, 0.0, "a.mp4");
    let lone = timeline.add_media_clip("L", ClipKind::Video, 0, 6.0, 2.0, 0.0, "a.mp4");
    timeline.clip_mut(top).unwrap().blend = openreel_engine::BlendMode::Multiply;
    timeline.clip_mut(lone).unwrap().blend = openreel_engine::BlendMode::Screen;
    let assets = [asset("a.mp4", 1920, 1080)];
    let found: Vec<_> = kinds(&inspect(&timeline, &assets))
        .into_iter()
        .filter(|(kind, _)| ["hidden-clip", "blend-over-black"].contains(kind))
        .collect();
    assert_eq!(found, vec![("blend-over-black", vec![lone])], "{base}");

    for id in [top, lone] {
        let clip = timeline.clip_mut(id).unwrap();
        clip.blend = openreel_engine::BlendMode::Normal;
        clip.chroma = Some(openreel_engine::ChromaKey::default());
    }
    let found: Vec<_> = kinds(&inspect(&timeline, &assets))
        .into_iter()
        .filter(|(kind, _)| ["hidden-clip", "blend-over-black", "key-over-black"].contains(kind))
        .collect();
    assert_eq!(found, vec![("key-over-black", vec![lone])], "{base}");
}

/// A masked picture shows the lane under it outside its shape, so that
/// lane is not hidden; a mask that keeps none of its picture is reported.
#[test]
fn a_masked_picture_shows_the_lane_under_it_and_one_that_keeps_nothing_is_reported() {
    use openreel_engine::{Mask, MaskShape};
    let mut timeline = Timeline::empty_project();
    let base = timeline.add_media_clip("B", ClipKind::Video, 1, 0.0, 4.0, 0.0, "a.mp4");
    let top = timeline.add_media_clip("T", ClipKind::Video, 0, 0.0, 4.0, 0.0, "a.mp4");
    let assets = [asset("a.mp4", 1920, 1080)];
    let found = |timeline: &Timeline| -> Vec<_> {
        kinds(&inspect(timeline, &assets))
            .into_iter()
            .filter(|(kind, _)| ["hidden-clip", "mask-clears-picture"].contains(kind))
            .collect()
    };
    assert_eq!(found(&timeline), vec![("hidden-clip", vec![base])]);
    timeline.clip_mut(top).unwrap().mask = Some(Mask::new(MaskShape::Heart));
    assert_eq!(found(&timeline), vec![]);

    // Moved off the picture: a circle 0.6 of the height across, its middle
    // a whole picture width away, misses it.
    timeline.clip_mut(top).unwrap().mask = Some(Mask {
        x: 1.0,
        ..Mask::new(MaskShape::Circle)
    });
    assert_eq!(found(&timeline), vec![("mask-clears-picture", vec![top])]);
    // Inverted over a rectangle that covers the picture, nothing is kept;
    // one that leaves a strip at the edge keeps it.
    let covering = Mask {
        width: 2.0,
        height: 1.1,
        invert: true,
        ..Mask::new(MaskShape::Rectangle)
    };
    timeline.clip_mut(top).unwrap().mask = Some(covering);
    assert_eq!(found(&timeline), vec![("mask-clears-picture", vec![top])]);
    timeline.clip_mut(top).unwrap().mask = Some(Mask {
        height: 0.9,
        ..covering
    });
    assert_eq!(found(&timeline), vec![]);
}

//! Ducking, captions and tightened silence.

use super::*;

#[test]
fn ducking_follows_transcript_speech_and_only_music_ducks_by_default() {
    let mut app = Editor::headless();
    let turn = |start: f64, end: f64, text: &str| sweep::TranscriptSegment {
        start,
        end,
        speaker: None,
        text: text.to_owned(),
        words: Vec::new(),
        session: None,
    };
    let asset = |path: &str, has_video: bool, transcript| BinAsset {
        info: MediaInfo {
            path: path.to_owned(),
            name: path.to_owned(),
            duration: 60.0,
            width: if has_video { 1280 } else { 0 },
            height: if has_video { 720 } else { 0 },
            fps: 30.0,
            has_video,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData {
            transcript,
            ..Default::default()
        },
    };
    app.assets.push(asset(
        "talk.mp4",
        true,
        vec![
            turn(10.0, 12.0, "Hello there, welcome back."),
            // Crowd ambience across 10 s: not speech.
            turn(20.0, 30.0, "LT"),
        ],
    ));
    app.assets.push(asset("song.wav", false, Vec::new()));
    app.timeline
        .add_media_clip("talk", ClipKind::Video, 0, 0.0, 30.0, 0.0, "talk.mp4");
    assert!(app.speech_map().is_empty(), "nothing ducks yet");

    // Music (a steady beat) ducks by default; a voiceover does not.
    app.assets[1].beats = Some(openreel_media::BeatGrid {
        bpm: 120.0,
        beats: vec![0.0, 0.5],
    });
    app.add_asset_range_to_timeline("song.wav", "music", None, 0.0, 0.0, 30.0);
    assert!(app.timeline.clips.last().unwrap().duck, "music ducks");
    app.assets[1].beats = None;
    app.add_asset_range_to_timeline("song.wav", "voiceover", None, 30.0, 0.0, 5.0);
    assert!(!app.timeline.clips.last().unwrap().duck, "voice does not");

    let speech = app.speech_map();
    assert_eq!(speech["talk.mp4"], vec![(10.0, 12.0)]);
}

#[test]
fn untranscribed_speech_is_read_from_the_waveform_at_its_own_level() {
    // A quiet voiceover whose sound ends 2 s before its container does.
    // Peaks were stretched over the container and judged against a fixed
    // 0.03, so speech landed late and the soft syllable was silence.
    let mut app = Editor::headless();
    let mut peaks = vec![0.0005_f32; 50];
    peaks.extend([0.06; 25]);
    peaks.extend([0.008; 25]); // a soft trailing syllable
    peaks.extend([0.0005; 50]);
    peaks.extend([0.06; 50]);
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "voice.wav".to_owned(),
            name: "voice.wav".to_owned(),
            duration: 6.0,
            width: 0,
            height: 0,
            fps: 0.0,
            has_video: false,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks,
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    });
    app.timeline
        .add_media_clip("voice", ClipKind::Audio, 0, 0.0, 6.0, 0.0, "voice.wav");
    let music = app
        .timeline
        .add_media_clip("music", ClipKind::Audio, 1, 0.0, 6.0, 0.0, "song.wav");
    app.timeline.clip_mut(music).expect("music clip").duck = true;
    assert_eq!(app.speech_map()["voice.wav"], vec![(1.0, 2.0), (3.0, 4.0)]);
    let state: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    let quiet: Vec<(f64, f64)> = state["assets"][0]["silenceRanges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|range| {
            (
                range["start"].as_f64().unwrap(),
                range["end"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(quiet, vec![(0.0, 1.0), (2.0, 3.0)]);
}

#[test]
fn adding_captions_maps_transcript_chunks_onto_a_timeline_clip() {
    let mut app = Editor::headless();
    let words = [
        ("One", 10.0, 10.3),
        ("two", 10.4, 10.7),
        ("three", 10.8, 11.2),
        ("four", 11.3, 11.7),
        ("five", 11.8, 12.1),
        ("six", 12.2, 12.5),
        ("seven", 12.6, 13.0),
        ("eight.", 13.1, 13.6),
    ]
    .into_iter()
    .map(|(text, start, end)| sweep::TranscriptWord {
        start,
        end,
        text: text.to_owned(),
        confidence: None,
        estimated: false,
    })
    .collect();
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/talk.mp4".to_owned(),
            name: "talk.mp4".to_owned(),
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
            transcript: vec![sweep::TranscriptSegment {
                start: 10.0,
                end: 14.0,
                speaker: Some("A".to_owned()),
                text: "One two three four five six seven eight.".to_owned(),
                words,
                session: None,
            }],
            ..Default::default()
        },
    });
    let clip = app.timeline.add_media_clip(
        "talk.mp4",
        ClipKind::Video,
        0,
        5.0,
        4.0,
        10.0,
        "/media/talk.mp4",
    );

    let added = app
        .add_captions_for_clip(clip, openreel_engine::TitleStyle::PopCaption)
        .expect("captions generated from transcript");
    assert!(added > 1);
    let captions: Vec<_> = app
        .timeline
        .clips
        .iter()
        .filter(|clip| clip.kind == ClipKind::Title)
        .collect();
    assert_eq!(captions.len(), added);
    assert!((captions[0].start - 5.0).abs() < 0.04);
    assert!(captions
        .iter()
        .all(|clip| clip.title_style == openreel_engine::TitleStyle::PopCaption));
    assert!(captions
        .iter()
        .all(|clip| clip.start >= 5.0 && clip.end() <= 9.04));
    // Each word lights when its source time plays: source 10.0 is at 5.0.
    let source_starts: HashMap<&str, f64> = [
        ("One", 10.0),
        ("two", 10.4),
        ("three", 10.8),
        ("four", 11.3),
        ("five", 11.8),
        ("six", 12.2),
        ("seven", 12.6),
        ("eight.", 13.1),
    ]
    .into_iter()
    .collect();
    for caption in &captions {
        let times = caption.word_times().expect("one time per caption word");
        for (word, time) in caption.name.split_whitespace().zip(times) {
            let expected = 5.0 + source_starts[word] - 10.0;
            assert!(
                (time - expected).abs() < 1e-9,
                "{word}: {time} vs {expected}"
            );
        }
    }
    let three = captions
        .iter()
        .find(|clip| clip.name.split_whitespace().any(|word| word == "three"))
        .unwrap();
    let index = three.name.split_whitespace().position(|w| w == "three");
    assert_eq!(three.active_word(5.85), index);
    assert_eq!(
        app.add_captions_for_clip(clip, openreel_engine::TitleStyle::PopCaption)
            .expect("second caption pass is idempotent"),
        0
    );

    // A piece that only catches the edges of two lines repeats neither.
    let sliver = app.timeline.add_media_clip(
        "talk.mp4",
        ClipKind::Video,
        0,
        20.0,
        0.5,
        12.9,
        "/media/talk.mp4",
    );
    assert_eq!(
        app.add_captions_for_clip(sliver, openreel_engine::TitleStyle::PopCaption)
            .expect("edge-only piece"),
        0
    );

    // A script is told what the pass did, and why a clip got nothing.
    let note = |app: &mut Editor, clip: ClipId| {
        let command = ScriptCommand::AddCaptionsForClip {
            clip,
            style: "pop-caption".to_owned(),
        };
        app.apply_edit(vec![command]).unwrap().0.join("\n")
    };
    let place = |app: &mut Editor, at: f64, source_in: f64| {
        app.timeline.add_media_clip(
            "talk.mp4",
            ClipKind::Video,
            0,
            at,
            4.0,
            source_in,
            "/media/talk.mp4",
        )
    };
    let fresh = place(&mut app, 30.0, 10.0);
    assert_eq!(
        note(&mut app, fresh),
        format!("clip {fresh}: added {added} captions")
    );
    assert_eq!(
        note(&mut app, clip),
        format!("clip {clip}: no captions added; {added} lines already captioned")
    );
    assert_eq!(
        note(&mut app, sliver),
        format!("clip {sliver}: no captions added; 2 lines left out, under half inside the clip")
    );
    let wordless = place(&mut app, 40.0, 0.0);
    assert_eq!(
        note(&mut app, wordless),
        format!("clip {wordless}: no captions added; no speech in its source range")
    );
}

#[test]
fn tightening_silence_replaces_dead_air_and_ripples_following_clips() {
    let mut app = Editor::headless();
    let mut peaks = vec![0.0; 25];
    peaks.extend([0.8; 25]);
    peaks.extend([0.0; 25]);
    peaks.extend([0.7; 25]);
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/talk.mp4".to_owned(),
            name: "talk.mp4".to_owned(),
            duration: 4.0,
            width: 854,
            height: 480,
            fps: 30.0,
            has_video: true,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks,
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    });
    let clip = app.timeline.add_media_clip(
        "talk.mp4",
        ClipKind::Video,
        0,
        2.0,
        2.0,
        0.0,
        "/media/talk.mp4",
    );
    let follower = app
        .timeline
        .add_clip("Place card", ClipKind::Title, 1, 5.0, 1.0, 0.0);
    {
        let clip = app.timeline.clip_mut(clip).unwrap();
        (clip.fade_in, clip.fade_out) = (0.2, 0.3);
        (clip.duck, clip.clean_voice, clip.smooth_fades) = (true, true, true);
        clip.volume = 0.5;
    }

    let pieces = app
        .tighten_clip_silence(clip, None, 0.35, 0.1)
        .expect("waveform peaks tighten the clip");
    assert_eq!(pieces, 2);
    let mut kept: Vec<_> = app
        .timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Video)
        .collect();
    kept.sort_by(|a, b| a.start.total_cmp(&b.start));
    assert_eq!(
        kept[0].id, clip,
        "the first kept phrase retains the clip id"
    );
    assert!((kept[0].start - 2.0).abs() < 0.04);
    assert!((kept[0].source_in - 0.4).abs() < 0.04);
    assert!((kept[0].duration - 0.7).abs() < 0.04);
    assert!((kept[1].start - kept[0].end()).abs() < 0.04);
    assert!((kept[1].source_in - 1.4).abs() < 0.04);
    assert!((kept[1].duration - 0.6).abs() < 0.04);
    for piece in &kept {
        assert_eq!(
            (
                piece.volume,
                piece.duck,
                piece.clean_voice,
                piece.smooth_fades
            ),
            (0.5, true, true, true),
            "every phrase sounds as the clip did"
        );
    }
    assert_eq!(
        [
            kept[0].fade_in,
            kept[0].fade_out,
            kept[1].fade_in,
            kept[1].fade_out
        ],
        [0.2, 0.0, 0.0, 0.3],
        "the fades stay at the outer ends"
    );
    assert!(
        (app.timeline.clip(follower).unwrap().start - 4.3).abs() < 0.04,
        "following material ripples left by the removed dead air"
    );
}

#[test]
fn tightening_silence_keeps_detached_audio_in_linked_phrases() {
    let mut app = Editor::headless();
    let mut peaks = vec![0.0; 25];
    peaks.extend([0.8; 25]);
    peaks.extend([0.0; 25]);
    peaks.extend([0.7; 25]);
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/talk.mp4".to_owned(),
            name: "talk.mp4".to_owned(),
            duration: 2.0,
            width: 854,
            height: 480,
            fps: 30.0,
            has_video: true,
            has_audio: true,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks,
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    });
    let video = app.timeline.add_media_clip(
        "talk.mp4",
        ClipKind::Video,
        0,
        0.0,
        2.0,
        0.0,
        "/media/talk.mp4",
    );
    let audio = app.timeline.detach_audio(video).expect("detach audio");

    assert_eq!(
        app.tighten_clip_silence(video, Some(0.03), 0.35, 0.1)
            .expect("tighten linked pair"),
        2
    );
    assert_eq!(
        app.timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Video)
            .count(),
        2
    );
    assert_eq!(
        app.timeline
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Audio)
            .count(),
        2
    );
    assert_eq!(app.timeline.clip(video).unwrap().linked_clip, Some(audio));
    for picture in app
        .timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Video)
    {
        let sound = app
            .timeline
            .clip(picture.linked_clip.expect("each phrase stays linked"))
            .unwrap();
        assert_eq!(sound.start, picture.start);
        assert_eq!(sound.duration, picture.duration);
        assert_eq!(sound.source_in, picture.source_in);
        assert!(!picture.embedded_audio);
    }
}

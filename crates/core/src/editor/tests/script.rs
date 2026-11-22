//! One script as one edit, the path `eddie run` and the live server take:
//! the request-time state, then every command or none. Also the console.

use openreel_engine::Property;

use super::*;

fn asset(path: &str, duration: f64, video: bool) -> BinAsset {
    BinAsset {
        info: openreel_media::MediaInfo {
            path: path.to_owned(),
            name: path.to_owned(),
            duration,
            width: if video { 320 } else { 0 },
            height: if video { 180 } else { 0 },
            fps: if video { 30.0 } else { 0.0 },
            has_video: video,
            has_audio: true,
            photo: false,
            alpha: false,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    }
}

fn run(app: &mut Editor, code: &str) -> Result<String, String> {
    let mut engine = ScriptEngine::new()?;
    let (output, commands) = engine.eval_checked(code, &app.script_state_json())?;
    app.apply_edit(commands)?;
    Ok(output)
}

#[test]
fn an_edit_that_leaves_clips_sharing_a_lane_is_refused() {
    let mut app = Editor::headless();
    app.assets.push(asset("a.mp4", 20.0, true));
    app.assets.push(asset("b.mp4", 20.0, true));
    run(
        &mut app,
        "addAssetRange('a.mp4', 0, 0, 5); addAssetRange('b.mp4', 5, 0, 5);",
    )
    .unwrap();
    let before = app.timeline.clone();
    let ids: Vec<ClipId> = before.clips.iter().map(|c| c.id).collect();
    let (a, b) = (ids[0], ids[1]);
    assert_eq!(before.clip(a).unwrap().track, before.clip(b).unwrap().track);

    // Moving b onto a, or trimming a over b, leaves both on one lane.
    for code in [
        format!("moveClip({b}, 2)"),
        format!("setClipLane({b}, 1); moveClip({b}, 2); setClipLane({b}, 0)"),
    ] {
        let error = run(&mut app, &code).unwrap_err();
        assert!(error.contains("would overlap on video lane 0"), "{error}");
        assert!(
            error.contains("a.mp4") && error.contains("b.mp4"),
            "{error}"
        );
        assert_eq!(app.timeline, before, "a refused edit changes nothing");
    }
    // Passing through an overlap on the way to a valid timeline is fine.
    run(&mut app, &format!("moveClip({b}, 0); moveClip({a}, 5)")).unwrap();
    assert_eq!(app.timeline.clip(b).unwrap().start, 0.0);
    assert_eq!(app.timeline.clip(a).unwrap().start, 5.0);
    // So is another lane.
    run(&mut app, &format!("setClipLane({a}, 1); moveClip({a}, 2)")).unwrap();

    // A clip sent days away would keep the loudness worker rendering.
    let error = run(&mut app, &format!("moveClip({a}, 1e12)")).unwrap_err();
    assert!(error.contains("supported timeline range"), "{error}");
}

#[test]
fn a_cutaway_moved_to_lane_zero_shows_over_the_base_clip() {
    let mut app = Editor::headless();
    app.assets.push(asset("talk.mp4", 20.0, true));
    app.assets.push(asset("broll.mp4", 20.0, true));
    run(
        &mut app,
        "var talk = addAssetRange('talk.mp4', 0, 0, 12); setClipLane(talk, 1); \
             var cut = addAssetRange('broll.mp4', 4, 2, 3); setClipLane(cut, 0); \
             setEmbeddedAudio(cut, false);",
    )
    .unwrap();
    let shown = |app: &Editor, t: f64| app.timeline.active_video(t).map(|c| c.name.clone());
    assert_eq!(shown(&app, 2.0).as_deref(), Some("talk.mp4"));
    assert_eq!(shown(&app, 5.0).as_deref(), Some("broll.mp4"));
    assert_eq!(shown(&app, 8.0).as_deref(), Some("talk.mp4"));
    let cut = app
        .timeline
        .clips
        .iter()
        .find(|c| c.name == "broll.mp4")
        .unwrap();
    assert!(!cut.embedded_audio);

    // Split the base and move its right piece by handle.
    run(
        &mut app,
        "var t = clips().filter(c => c.name == 'talk.mp4')[0].id; \
             var right = splitClip(t, 9); moveClip(right, 29);",
    )
    .unwrap();
    let spans = |app: &Editor, name: &str| {
        let mut found: Vec<_> = app
            .timeline
            .clips
            .iter()
            .filter(|c| c.name == name)
            .map(|c| (c.start, c.source_in, c.duck))
            .collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found
    };
    assert_eq!(
        spans(&app, "talk.mp4"),
        [(0.0, 0.0, false), (29.0, 9.0, false)]
    );

    // Music ducks by default; insertTime opens room on every lane.
    let mut bed = asset("bed.mp3", 60.0, false);
    bed.beats = Some(openreel_media::BeatGrid {
        bpm: 120.0,
        beats: vec![0.0, 0.5],
    });
    app.assets.push(bed);
    run(
        &mut app,
        "addAssetRange('bed.mp3', 0, 0, 20); insertTime(2, 5);",
    )
    .unwrap();
    assert_eq!(spans(&app, "bed.mp3"), [(0.0, 0.0, true), (7.0, 2.0, true)]);
    assert_eq!(
        spans(&app, "talk.mp4"),
        [(0.0, 0.0, false), (7.0, 2.0, false), (34.0, 9.0, false)]
    );
    assert_eq!(spans(&app, "broll.mp4"), [(9.0, 2.0, false)]);
}

#[test]
fn additions_return_handles_for_later_commands() {
    let mut app = Editor::headless();
    app.assets.push(asset("ready.mp4", 10.0, true));
    let keep = app
        .timeline
        .add_clip("Keep", ClipKind::Title, 0, 0.0, 1.0, 0.0);
    // A deleted clip makes "highest ID + 1" a wrong guess.
    let gone = app
        .timeline
        .add_clip("Gone", ClipKind::Title, 0, 1.0, 1.0, 0.0);
    app.timeline.remove_clip(gone);
    run(
        &mut app,
        "var shot = addAssetRange('ready.mp4', 2, 0, 4); setFade(shot, 0, 1.5); \
             var card = addTitle('Card', 0, 1); moveClip(card, 6);",
    )
    .unwrap();
    let find = |name: &str| app.timeline.clips.iter().find(|c| c.name == name).unwrap();
    assert_eq!(
        (find("ready.mp4").start, find("ready.mp4").fade_out),
        (2.0, 1.5)
    );
    assert_eq!(find("Card").start, 6.0);
    assert_eq!(app.timeline.clip(keep).unwrap().start, 0.0);
}

#[test]
fn a_failed_script_restores_the_entire_edit() {
    for (state, failing_command) in [
        (AssetState::Ready, "addAsset('missing.mp4', 0)"),
        (AssetState::Ready, "addAssetRange('ready.mp4', 0, 11, 1)"),
        (AssetState::Processing, "addAsset('ready.mp4', 0)"),
        (AssetState::Ready, "moveClip(999, 4)"),
        (
            AssetState::Ready,
            "addTitle('Overflow', Number.MAX_VALUE, 2)",
        ),
        (AssetState::Ready, "exportTo('/tmp/out.mp4')"),
    ] {
        let mut app = Editor::headless();
        let mut ready = asset("ready.mp4", 10.0, true);
        ready.state = state;
        app.assets.push(ready);
        let id = app
            .timeline
            .add_clip("Keep", ClipKind::Title, 0, 0.0, 5.0, 0.0);
        app.selected = Some(id);
        app.playhead = 1.0;
        app.playing = true;
        let before = app.timeline.clone();
        let zoom = app.viewport.px_per_sec;
        let result = run(
            &mut app,
            &format!(
                "deleteClip({id}); addTitle('Transient', 0, 2); pause(); seek(0); \
                     setMagnet(false); setMusicBedMuted(false); setZoom(123); {failing_command}"
            ),
        );
        assert!(result.is_err(), "{failing_command}");
        assert_eq!(app.timeline, before, "{failing_command}");
        assert_eq!(app.selected, Some(id));
        assert_eq!(app.playhead, 1.0);
        assert!(app.playing);
        assert!(app.viewport.magnet && app.mute_music_beds);
        assert_eq!(app.viewport.px_per_sec, zoom);
    }
}

/// An error an agent can act on names what would have worked.
#[test]
fn an_unknown_title_style_lists_the_styles() {
    let mut app = Editor::headless();
    let title = app
        .timeline
        .add_clip("Keep", ClipKind::Title, 0, 0.0, 5.0, 0.0);
    for script in [
        "addTitle('New', 6, 2, 'bogus')".to_owned(),
        format!("setTitleStyle({title}, 'bogus')"),
        format!("addCaptionsForClip({title}, 'bogus')"),
    ] {
        let error = run(&mut app, &script).unwrap_err();
        for style in openreel_engine::TitleStyle::ALL {
            assert!(error.contains(style.as_str()), "{script}: {error}");
        }
    }
}

/// A script that computed a time before zero, or an end before a
/// start, has a mistake: it hears about it instead of getting an edit
/// it did not ask for.
#[test]
fn arguments_out_of_range_are_refused_not_moved_into_range() {
    let mut app = Editor::headless();
    app.assets.push(asset("ready.mp4", 10.0, true));
    let id = app
        .timeline
        .add_clip("Keep", ClipKind::Title, 0, 2.0, 5.0, 0.0);
    let before = app.timeline.clone();
    for (script, error) in [
        (
            format!("moveClip({id}, -5)"),
            "moveClip needs a start of 0 or later, not -5",
        ),
        (
            "addTitle('Z', -1, 1)".to_owned(),
            "addTitle needs a time of 0 or later, not -1",
        ),
        (
            "addTitle('Z', 8, 0)".to_owned(),
            "addTitle needs a positive length, not 0",
        ),
        (
            "addAsset('ready.mp4', -2)".to_owned(),
            "addAsset needs a time of 0 or later, not -2",
        ),
        (
            "addAssetRange('ready.mp4', 8, -1, 1)".to_owned(),
            "addAssetRange needs a source start of 0 or later, not -1",
        ),
        (
            "addAssetRange('ready.mp4', 8, 0, -3)".to_owned(),
            "addAssetRange needs a positive length, not -3",
        ),
        (
            "insertTime(-5, 1)".to_owned(),
            "insertTime needs a time of 0 or later, not -5",
        ),
        (
            format!("trimClip({id}, 6, 4)"),
            "at 4 s, not after its start at 6 s",
        ),
    ] {
        let refused = run(&mut app, &script).unwrap_err();
        assert!(refused.contains(error), "{script}: {refused}");
        assert_eq!(app.timeline, before, "{script}");
    }
    // The edges themselves are still fine.
    run(
        &mut app,
        &format!("trimClip({id}, 3, 4); moveClip({id}, 0)"),
    )
    .unwrap();
    let clip = app.timeline.clip(id).unwrap();
    assert_eq!((clip.start, clip.duration), (0.0, 1.0));
}

#[test]
fn scripts_read_utterances_and_snap_ranges_out_of_them() {
    let mut app = Editor::headless();
    let mut talk = asset("talk.mp4", 20.0, true);
    let turn = |start: f64, end: f64, text: &str| sweep::TranscriptSegment {
        start,
        end,
        speaker: Some("A".to_owned()),
        text: text.to_owned(),
        words: Vec::new(),
        session: None,
    };
    talk.sweep.transcript = vec![
        turn(1.0, 3.0, "First thought."),
        turn(3.05, 4.0, "Still me"),
        turn(6.0, 9.0, "And a second one."),
    ];
    app.assets.push(talk);
    let output = run(
            &mut app,
            "const span = (r) => [r.start, r.end]; \
             return JSON.stringify([utterances('talk.mp4').map(span), \
             span(snapToSpeech('talk.mp4', 2, 7)), span(snapToSpeech('talk.mp4', 4.5, 5.5)), \
             span(snapToSpeech('talk.mp4', 0.95, 9.05)), span(snapToSpeech('talk.mp4', 3.5, 3.6))])",
        )
        .unwrap();
    type Span = (f64, f64);
    let (groups, inside, pause, handles, crowded): (Vec<Span>, Span, Span, Span, Span) =
        serde_json::from_str(&output).unwrap();
    // A finished thought is its own utterance even when the same
    // speaker goes on 50 ms later.
    assert_eq!(groups, [(1.0, 3.0), (3.05, 4.0), (6.0, 9.0)]);
    // Both edges inside speech: out to the handles, never past a
    // neighbour's edge.
    let near = |a: Span, b: Span| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
    assert!(near(inside, (0.88, 9.12)), "{inside:?}");
    // Already in a pause: unchanged.
    assert_eq!(pause, (4.5, 5.5));
    // Inside a handle only: widened to the full handle.
    assert!(near(handles, (0.88, 9.12)), "{handles:?}");
    // The lead-in handle stops at the previous utterance's end.
    assert!(near(crowded, (3.0, 4.12)), "{crowded:?}");
}

#[test]
fn console_history_recalls_lines_and_ends_on_an_empty_one() {
    let mut editor = Editor::headless();
    assert_eq!(editor.recall_console(true), None, "nothing was run yet");
    for line in ["1", "2", "2"] {
        editor.run_console(line.to_owned());
    }
    assert_eq!(editor.recall_console(true).as_deref(), Some("2"));
    assert_eq!(
        editor.recall_console(true).as_deref(),
        Some("1"),
        "a line run twice in a row is kept once"
    );
    assert_eq!(
        editor.recall_console(true).as_deref(),
        Some("1"),
        "Up stops at the oldest"
    );
    assert_eq!(editor.recall_console(false).as_deref(), Some("2"));
    assert_eq!(
        editor.recall_console(false).as_deref(),
        Some(""),
        "Down past the newest"
    );
    editor.run_console("3".to_owned());
    assert_eq!(
        editor.recall_console(true).as_deref(),
        Some("3"),
        "running a line starts recall from the newest again"
    );
}

#[test]
fn undo_and_redo_step_through_console_edits() {
    let mut app = Editor::headless();
    let track = |app: &mut Editor| {
        app.history
            .track(&app.timeline, app.selected, 0.0, false, false);
    };
    app.run_console("addTitle('One', 0, 1);".to_owned());
    track(&mut app);
    app.run_console("addTitle('Two', 2, 1); addTitle('Three', 4, 1);".to_owned());
    track(&mut app);
    assert_eq!(app.timeline.clips.len(), 3);

    app.perform(Action::Undo);
    assert_eq!(app.timeline.clips.len(), 1, "one console run is one step");
    assert_eq!(app.status, "Undid the last edit.");
    app.perform(Action::Undo);
    assert!(app.timeline.clips.is_empty());
    app.perform(Action::Undo);
    assert_eq!(app.status, "Nothing to undo.");
    app.perform(Action::Redo);
    app.perform(Action::Redo);
    assert_eq!(app.timeline.clips.len(), 3);
    app.perform(Action::ClearTimeline);
    track(&mut app);
    app.perform(Action::Undo);
    assert_eq!(
        app.timeline.clips.len(),
        3,
        "clearing the timeline is undoable"
    );
}

#[test]
fn titles_video_and_sound_keep_to_lanes_of_their_own_kind() {
    let mut app = Editor::headless();
    app.assets.push(asset("a.mp4", 20.0, true));
    app.assets.push(asset("music.wav", 20.0, false));
    run(
        &mut app,
        "addAssetRange('a.mp4', 0, 0, 5); addAssetRange('music.wav', 0, 0, 5); \
         addTitle('Over the picture', 1, 2);",
    )
    .unwrap();
    let lane = |app: &Editor, name: &str| {
        let clip = app.timeline.clips.iter().find(|c| c.name == name).unwrap();
        (clip.kind, clip.track)
    };
    // Each is on lane 0 of its kind: a title over a picture needs no
    // second lane.
    assert_eq!(lane(&app, "a.mp4"), (ClipKind::Video, 0));
    assert_eq!(lane(&app, "music.wav"), (ClipKind::Audio, 0));
    assert_eq!(lane(&app, "Over the picture"), (ClipKind::Title, 0));
    let id = |app: &Editor, name: &str| {
        app.timeline
            .clips
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .id
    };

    // A lane number counts within the clip's kind and grows its lanes.
    let video = id(&app, "a.mp4");
    run(&mut app, &format!("setClipLane({video}, 1)")).unwrap();
    assert_eq!(lane(&app, "a.mp4"), (ClipKind::Video, 1));
    assert_eq!(
        (
            app.timeline.lanes.titles,
            app.timeline.lanes.video,
            app.timeline.lanes.audio
        ),
        (1, 2, 1)
    );
    let tracks = run(&mut app, "return clips().map(c => c.kind + ' ' + c.track);").unwrap();
    assert!(
        tracks.contains("video 1") && tracks.contains("audio 0"),
        "{tracks}"
    );
}

#[test]
fn transforms_backgrounds_and_formats_change_only_what_is_asked() {
    let mut app = Editor::headless();
    app.assets.push(asset("wide.mp4", 20.0, true));
    app.assets.push(asset("song.wav", 20.0, false));
    run(
        &mut app,
        "addAssetRange('wide.mp4', 0, 0, 5); addAssetRange('song.wav', 0, 0, 5)",
    )
    .unwrap();
    let video = app.timeline.clips[0].id;
    let song = app.timeline.clips[1].id;
    let transform = |app: &Editor| app.timeline.clip(video).unwrap().transform;

    // Each call changes the fields it names and keeps the rest.
    run(
        &mut app,
        &format!("setTransform({video}, {{x: 0.25, flipH: true}})"),
    )
    .unwrap();
    run(
        &mut app,
        &format!("setTransform({video}, {{rotation: 90}})"),
    )
    .unwrap();
    let t = transform(&app);
    assert_eq!(
        (t.x, t.flip_h, t.rotation, t.scale),
        (0.25, true, 90.0, 1.0)
    );
    let read = run(&mut app, "return clips()[0].transform").unwrap();
    let read: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert_eq!(
        (read["x"].as_f64(), read["flipH"].as_bool()),
        (Some(0.25), Some(true))
    );
    run(&mut app, &format!("setTransform({video}, null)")).unwrap();
    assert!(transform(&app).is_identity());

    // On a 9:16 canvas (180x320) the 320x180 picture fits 180 wide; Fill
    // scales it until it is 320 tall: 320 / (180 * 180 / 320).
    run(&mut app, "setFormat('9:16')").unwrap();
    assert_eq!(app.timeline.format, openreel_engine::Format::Portrait);
    assert_eq!(app.settings.format, openreel_engine::Format::Portrait);
    assert_eq!(app.canvas(), (180, 320));
    run(
        &mut app,
        &format!("setTransform({video}, {{scale: 'fill'}})"),
    )
    .unwrap();
    let fill = 320.0 / (180.0 * 180.0 / 320.0);
    assert!(
        (transform(&app).scale - fill).abs() < 1e-9,
        "{:?}",
        transform(&app)
    );
    run(&mut app, &format!("setBackground({video}, 'blur')")).unwrap();
    assert_eq!(
        app.timeline.clip(video).unwrap().background,
        openreel_engine::Background::Blur
    );
    run(&mut app, &format!("setBlend({video}, 'Soft light')")).unwrap();
    let read = run(
        &mut app,
        &format!("return state().clips.find(c => c.id == {video}).blend"),
    )
    .unwrap();
    assert_eq!(read, "soft-light");
    assert_eq!(
        app.timeline.clip(video).unwrap().blend,
        openreel_engine::BlendMode::SoftLight
    );
    // {} keys green; later changes keep what they do not name.
    run(&mut app, &format!("setChroma({video}, {{}})")).unwrap();
    assert_eq!(
        app.timeline.clip(video).unwrap().chroma,
        Some(openreel_engine::ChromaKey::default())
    );
    run(
        &mut app,
        &format!("setChroma({video}, {{color: '#0047bb', strength: 0.7}})"),
    )
    .unwrap();
    let read = run(
        &mut app,
        &format!("const k = state().clips.find(c => c.id == {video}).chroma; return [k.color, k.strength, k.softness]"),
    )
    .unwrap();
    assert_eq!(read, r##"["#0047BB",0.7,0.3]"##);
    run(&mut app, &format!("setChroma({video}, null)")).unwrap();
    assert_eq!(app.timeline.clip(video).unwrap().chroma, None);

    // {} masks a circle; a shape name is read in any case, and later
    // changes keep what they do not name.
    run(&mut app, &format!("setMask({video}, {{}})")).unwrap();
    assert_eq!(
        app.timeline.clip(video).unwrap().mask,
        Some(openreel_engine::Mask::default())
    );
    run(
        &mut app,
        &format!("setMask({video}, {{shape: 'Heart', x: 0.2, invert: true}})"),
    )
    .unwrap();
    run(&mut app, &format!("setMask({video}, {{feather: 0.5}})")).unwrap();
    let read = run(
        &mut app,
        &format!("const m = state().clips.find(c => c.id == {video}).mask; return [m.shape, m.x, m.invert, m.feather, m.width]"),
    )
    .unwrap();
    assert_eq!(read, r#"["heart",0.2,true,0.5,0.6]"#);
    run(&mut app, &format!("setMask({video}, null)")).unwrap();
    assert_eq!(app.timeline.clip(video).unwrap().mask, None);

    // Refused whole: nothing before the failing command stays applied,
    // the format included.
    let before = app.timeline.clone();
    for (code, says) in [
        (
            format!("setFormat('square'); setTransform({video}, {{zoom: 2}})"),
            "no 'zoom'",
        ),
        (
            format!("setTransform({video}, {{cropLeft: 0.5, cropRight: 0.5}})"),
            "keep 5%",
        ),
        (format!("setTransform({video}, {{scale: NaN}})"), "finite"),
        (
            format!("setTransform({video}, {{scale: 'huge'}})"),
            "'fit' or 'fill'",
        ),
        (
            format!("setTransform({song}, {{scale: 2}})"),
            "needs a video clip",
        ),
        (format!("setBackground({video}, 'pink')"), "black or blur"),
        (format!("setBlend({video}, 'glow')"), "use normal, darken"),
        (format!("setBlend({song}, 'screen')"), "needs a video clip"),
        (format!("setChroma({video}, {{spill: 1.5}})"), "from 0 to 1"),
        (format!("setChroma({video}, {{color: 'lime'}})"), "#RRGGBB"),
        (format!("setChroma({video}, {{hue: 0.3}})"), "no 'hue'"),
        (format!("setChroma({song}, {{}})"), "needs a video clip"),
        (
            format!("setMask({video}, {{shape: 'oval'}})"),
            "one of split",
        ),
        (
            format!("setMask({video}, {{width: 0}})"),
            "width and height from",
        ),
        (format!("setMask({video}, {{x: NaN}})"), "finite"),
        (format!("setMask({video}, {{size: 1}})"), "no 'size'"),
        (format!("setMask({song}, {{}})"), "needs a video clip"),
        ("setFormat('3:2')".to_owned(), "unknown format"),
    ] {
        let error = run(&mut app, &code).unwrap_err();
        assert!(error.contains(says), "{code}: {error}");
        assert_eq!(app.timeline, before, "{code}");
        assert_eq!(
            app.settings.format,
            openreel_engine::Format::Portrait,
            "{code}"
        );
    }
}

#[test]
fn color_changes_only_what_is_asked_and_refuses_what_renderers_cannot_draw() {
    use openreel_engine::{ColorAdjust, Filter};
    let mut app = Editor::headless();
    app.assets.push(asset("wide.mp4", 20.0, true));
    app.assets.push(asset("song.wav", 20.0, false));
    run(
        &mut app,
        "addAssetRange('wide.mp4', 0, 0, 5); addAssetRange('song.wav', 0, 0, 5)",
    )
    .unwrap();
    let video = app.timeline.clips[0].id;
    let song = app.timeline.clips[1].id;
    let color = |app: &Editor| app.timeline.clip(video).unwrap().color;

    run(
        &mut app,
        &format!("setColor({video}, {{saturation: -0.5, filter: 'Warm'}})"),
    )
    .unwrap();
    run(
        &mut app,
        &format!("setColor({video}, {{exposure: 1.5, strength: 0.4}})"),
    )
    .unwrap();
    let c = color(&app);
    assert_eq!(
        (c.saturation, c.filter, c.exposure, c.strength, c.contrast),
        (-0.5, Filter::Warm, 1.5, 0.4, 0.0)
    );
    let read = run(&mut app, "return clips()[0].color").unwrap();
    let read: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert_eq!(
        (read["filter"].as_str(), read["exposure"].as_f64()),
        (Some("warm"), Some(1.5))
    );
    // A graded clip saves its colour; an ungraded one saves nothing.
    let saved = serde_json::to_value(app.timeline.clip(video).unwrap()).unwrap();
    assert_eq!(saved["color"]["filter"], "warm");
    let plain = serde_json::to_value(app.timeline.clip(song).unwrap()).unwrap();
    assert!(plain.get("color").is_none(), "{plain}");

    let before = app.timeline.clone();
    for (code, says) in [
        (format!("setColor({video}, {{hue: 0.2}})"), "no 'hue'"),
        (format!("setColor({video}, {{contrast: 1.5}})"), "-1 to 1"),
        (
            format!("setColor({video}, {{exposure: -3}})"),
            "-2 to 2 stops",
        ),
        (format!("setColor({video}, {{tint: NaN}})"), "finite"),
        (format!("setColor({video}, {{filter: 'lut'}})"), "vintage"),
        (format!("setColor({video}, 'warm')"), "needs an object"),
        (
            format!("setColor({song}, {{contrast: 0.2}})"),
            "needs a video clip",
        ),
    ] {
        let error = run(&mut app, &code).unwrap_err();
        assert!(error.contains(says), "{code}: {error}");
        assert_eq!(app.timeline, before, "{code}");
    }

    run(&mut app, &format!("setColor({video}, null)")).unwrap();
    assert_eq!(color(&app), ColorAdjust::default());
}

#[test]
fn text_changes_only_what_is_asked_and_null_goes_back_to_the_style() {
    use openreel_engine::{Rgb, TextLook};
    let mut app = Editor::headless();
    app.assets.push(asset("wide.mp4", 20.0, true));
    run(
        &mut app,
        "addAssetRange('wide.mp4', 0, 0, 5); addTitle('Hello', 0, 2)",
    )
    .unwrap();
    let video = app.timeline.clips[0].id;
    let title = app.timeline.clips[1].id;
    let text = |app: &Editor| app.timeline.clip(title).unwrap().text;

    run(
        &mut app,
        &format!("setText({title}, {{size: 1.5, color: '#ffd600', x: -0.25}})"),
    )
    .unwrap();
    run(
        &mut app,
        &format!("setText({title}, {{boxColor: '#36F', outline: 0.1}})"),
    )
    .unwrap();
    let t = text(&app);
    assert_eq!(
        (t.size, t.color, t.x, t.box_color, t.outline, t.y),
        (
            1.5,
            Some(Rgb(255, 214, 0)),
            -0.25,
            Some(Rgb(0x33, 0x66, 0xFF)),
            Some(0.1),
            0.0
        )
    );
    let read = run(&mut app, "return clips()[1].text").unwrap();
    let read: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert_eq!(
        (read["color"].as_str(), read["boxOpacity"].is_null()),
        (Some("#FFD600"), true)
    );
    // A field set to null goes back to the style's.
    run(&mut app, &format!("setText({title}, {{color: null}})")).unwrap();
    assert_eq!(text(&app).color, None);
    assert_eq!(text(&app).size, 1.5, "the rest stay");

    let before = app.timeline.clone();
    for (code, says) in [
        (format!("setText({title}, {{font: 'Arial'}})"), "no 'font'"),
        (format!("setText({title}, {{size: 5}})"), "0.25 to 4"),
        (
            format!("setText({title}, {{outline: 0.5}})"),
            "outline of 0 to 0.2",
        ),
        (format!("setText({title}, {{x: 1.5}})"), "-1 to 1"),
        (format!("setText({title}, {{boxOpacity: NaN}})"), "finite"),
        (format!("setText({title}, {{color: 'red'}})"), "'#RRGGBB'"),
        (format!("setText({title}, 'big')"), "needs an object"),
        (format!("setText({video}, {{size: 2}})"), "needs a title"),
    ] {
        let error = run(&mut app, &code).unwrap_err();
        assert!(error.contains(says), "{code}: {error}");
        assert_eq!(app.timeline, before, "{code}");
    }

    // A styled title saves its text look; null resets it all.
    let saved = serde_json::to_value(app.timeline.clip(title).unwrap()).unwrap();
    assert_eq!(saved["text"]["boxColor"], "#3366FF");
    run(&mut app, &format!("setText({title}, null)")).unwrap();
    assert_eq!(text(&app), TextLook::default());
    let saved = serde_json::to_value(app.timeline.clip(title).unwrap()).unwrap();
    assert!(saved.get("text").is_none(), "{saved}");
}

#[test]
fn speed_keeps_the_source_range_and_refuses_what_it_cannot_play() {
    let mut app = Editor::headless();
    app.assets.push(asset("wide.mp4", 20.0, true));
    app.assets.push(asset("song.wav", 20.0, false));
    run(
        &mut app,
        "var a = addAssetRange('wide.mp4', 0, 2, 4); addAssetRange('wide.mp4', 6, 0, 2); \
         detachAudio(a); setSpeed(a, 2)",
    )
    .unwrap();
    let video = app.timeline.clips[0].id;
    let partner = app.timeline.clip(video).unwrap().linked_clip.unwrap();
    for id in [video, partner] {
        let clip = app.timeline.clip(id).unwrap();
        assert_eq!(
            (clip.duration, clip.speed, clip.source_end()),
            (2.0, 2.0, 6.0)
        );
    }
    let read = run(
        &mut app,
        &format!("return clips().find(c => c.id == {video}).speed"),
    )
    .unwrap();
    assert_eq!(read, "2");
    // A sped-up clip saves its speed; one at normal speed saves nothing.
    let saved = serde_json::to_value(app.timeline.clip(video).unwrap()).unwrap();
    assert_eq!(saved["speed"], 2.0);
    let next = app.timeline.clips[1].id;
    let plain = serde_json::to_value(app.timeline.clip(next).unwrap()).unwrap();
    assert!(plain.get("speed").is_none(), "{plain}");

    let before = app.timeline.clone();
    for (code, says) in [
        (format!("setSpeed({video}, 5)"), "0.25 to 4"),
        (format!("setSpeed({video}, 0.1)"), "0.25 to 4"),
        (format!("setSpeed({video}, NaN)"), "finite"),
        (
            "setSpeed(addTitle('Hi', 10, 1), 2)".to_owned(),
            "video or audio",
        ),
        ("setSpeed(999, 2)".to_owned(), "no clip 999"),
        // At 0.5x the four source seconds take 8 s and would run into the
        // clip at 6 s; the whole run is refused.
        (format!("setSpeed({video}, 0.5)"), "overlap"),
    ] {
        let error = run(&mut app, &code).unwrap_err();
        assert!(error.contains(says), "{code}: {error}");
        assert_eq!(app.timeline, before, "{code}");
    }
    // Moving the next clip out of the way first makes room in the same run.
    run(
        &mut app,
        &format!("moveClip({next}, 9); setSpeed({video}, 0.5)"),
    )
    .unwrap();
    assert_eq!(app.timeline.clip(video).unwrap().duration, 8.0);
}

#[test]
fn a_photo_lasts_three_seconds_and_trims_to_any_length() {
    let dir = std::env::temp_dir().join(format!("eddie-photo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let photo = dir.join("photo.jpg");
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=green:s=64x36",
        ])
        .args(["-frames:v", "1"])
        .arg(&photo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let mut app = Editor::headless();
    app.spawn_import(vec![photo.clone()]);
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    while app.import_rx.is_some() || !app.import_queue.is_empty() {
        app.poll_imports();
        assert!(Instant::now() < deadline, "import timed out");
        thread::sleep(std::time::Duration::from_millis(20));
    }
    let asset = &app.assets[0];
    assert!(
        asset.state == AssetState::Ready && asset.info.photo,
        "{}",
        app.status
    );

    // Whole, it lasts CapCut's 3 s; a range is only a length.
    run(
        &mut app,
        "addAsset('photo.jpg', 1); addAssetRange('photo.jpg', 6, 40, 0.5)",
    )
    .unwrap();
    let (whole, range) = (app.timeline.clips[0].clone(), &app.timeline.clips[1]);
    assert_eq!(
        (whole.start, whole.duration, whole.source_in, whole.photo),
        (1.0, 3.0, 0.0, true)
    );
    assert_eq!(
        (range.duration, range.source_in, range.photo),
        (0.5, 0.0, true)
    );

    // Either edge goes as far as it likes: no source runs out.
    let id = whole.id;
    run(&mut app, &format!("trimClip({id}, 0, 5.5)")).unwrap();
    let trimmed = app.timeline.clip(id).unwrap();
    assert_eq!(
        (trimmed.start, trimmed.end(), trimmed.source_in),
        (0.0, 5.5, 0.0)
    );
    let error = run(&mut app, &format!("setSpeed({id}, 2)")).unwrap_err();
    assert!(error.contains("is a photo"), "{error}");
    let path = photo.to_string_lossy().into_owned();
    assert!(!app.queue_curation(path), "nothing to curate");

    // A saved project keeps them photos.
    app.save_project_to(&dir.join("photos")).unwrap();
    let mut reopened = Editor::headless();
    reopened.open_project(&dir.join("photos.eddie")).unwrap();
    assert_eq!(reopened.timeline, app.timeline);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scripts_duplicate_freeze_and_ripple_delete_through_handles() {
    let mut app = Editor::headless();
    app.assets.push(asset("a.mp4", 20.0, true));
    run(
        &mut app,
        "var a = addAssetRange('a.mp4', 0, 2, 4); var b = duplicateClip(a); \
         var f = freezeFrame(b, '5s', 1); setOpacity(f, 0.5); addTitle('Over a', 1, 2);",
    )
    .unwrap();
    let find = |app: &Editor, test: &dyn Fn(&Clip) -> bool| {
        let mut found: Vec<Clip> = app
            .timeline
            .clips
            .iter()
            .filter(|c| test(c))
            .cloned()
            .collect();
        found.sort_by(|x, y| x.start.total_cmp(&y.start));
        found
    };
    let freeze = &find(&app, &|c| c.freeze)[0];
    // The copy runs 4..8 from source 2, so 5 s shows source 3.
    assert_eq!(
        (freeze.start, freeze.source_in, freeze.opacity),
        (5.0, 3.0, 0.5)
    );
    let pictures = find(&app, &|c| c.kind == ClipKind::Video && !c.freeze);
    let spans: Vec<(f64, f64)> = pictures.iter().map(|c| (c.start, c.end())).collect();
    assert_eq!(spans, [(0.0, 4.0), (4.0, 5.0), (6.0, 9.0)]);

    let first = pictures[0].id;
    let (notes, _) = {
        let mut engine = ScriptEngine::new().unwrap();
        let (_, commands) = engine
            .eval_checked(&format!("rippleDelete({first})"), &app.script_state_json())
            .unwrap();
        app.apply_edit(commands).unwrap()
    };
    assert!(
        notes.iter().any(|note| note.contains("removed 1 title")),
        "{notes:?}"
    );
    let spans: Vec<(f64, f64)> = find(&app, &|c| c.kind == ClipKind::Video)
        .iter()
        .map(|c| (c.start, c.end()))
        .collect();
    assert_eq!(spans, [(0.0, 1.0), (1.0, 2.0), (2.0, 5.0)]);

    let freeze = find(&app, &|c| c.freeze)[0].id;
    let error = run(&mut app, &format!("freezeFrame({freeze}, 1.5)")).unwrap_err();
    assert!(error.contains("already a still"), "{error}");
}

#[test]
fn a_transition_needs_a_clip_before_it_and_inspect_says_where_it_falls_short() {
    let mut app = Editor::headless();
    app.assets.push(asset("wide.mp4", 10.0, true));
    // 2-4 s of the file, then 0.6 s from 4 s straight after it, with a 1 s
    // dip.
    run(
        &mut app,
        "var a = addAssetRange('wide.mp4', 0, 2, 2); \
         var b = addAssetRange('wide.mp4', 2, 4, 0.6); setTransition(b, 'dip-black', 1)",
    )
    .unwrap();
    let (a, b) = (app.timeline.clips[0].id, app.timeline.clips[1].id);
    let read = |app: &mut Editor| {
        run(
            app,
            &format!("return clips().find(c => c.id == {b}).transition"),
        )
        .unwrap()
    };
    assert_eq!(read(&mut app), r#"{"duration":1,"kind":"dip-black"}"#);
    let saved = serde_json::to_value(app.timeline.clip(b).unwrap()).unwrap();
    assert_eq!(saved["transition"]["kind"], "dip-black");
    let plain = serde_json::to_value(app.timeline.clip(a).unwrap()).unwrap();
    assert!(plain.get("transition").is_none(), "{plain}");
    let findings = |app: &Editor| {
        inspect::inspect(&app.timeline, app.assets.iter().chain(&app.graphics))
            .into_iter()
            .filter(|finding| finding.kind == "transition")
            .map(|finding| finding.message)
            .collect::<Vec<_>>()
    };
    // Each side takes at most half of its own clip: 0.3 s of the 0.6 s
    // clip after the cut, and as much before it.
    assert_eq!(
        findings(&app),
        [format!(
            "Clip {b}'s 1.00 s transition plays 0.60 s: each side takes at most half of its clip."
        )]
    );

    // A label names a kind too; without a length it takes 0.5 s.
    run(&mut app, &format!("setTransition({b}, 'Wipe left')")).unwrap();
    assert_eq!(read(&mut app), r#"{"duration":0.5,"kind":"wipe-left"}"#);
    assert!(findings(&app).is_empty(), "{:?}", findings(&app));
    run(&mut app, &format!("setTransition({b}, null)")).unwrap();
    assert_eq!(read(&mut app), "null");

    let before = app.timeline.clone();
    for (code, says) in [
        (format!("setTransition({a}, 'mix')"), "no clip right before"),
        (format!("setTransition({b}, 'spin')"), "use mix, dip-black"),
        (format!("setTransition({b}, 'mix', 6)"), "0.1 to 5"),
        (format!("setTransition({b}, 'mix', NaN)"), "finite"),
        (format!("setTransition({b})"), "needs a kind"),
        (
            "setTransition(addTitle('Hi', 10, 1), 'mix')".to_owned(),
            "needs a video clip",
        ),
    ] {
        let error = run(&mut app, &code).unwrap_err();
        assert!(error.contains(says), "{code}: {error}");
        assert_eq!(app.timeline, before, "{code}");
    }

    // The file's last 2 s, then its first 2 s, with a 1 s mix: the first
    // file runs out 0.5 s before the mix ends and the second starts 0.5 s
    // after it begins.
    run(
        &mut app,
        "addAssetRange('wide.mp4', 5, 8, 2); \
         setTransition(addAssetRange('wide.mp4', 7, 0, 2), 'mix', 1)",
    )
    .unwrap();
    let (d, e) = (app.timeline.clips[2].id, app.timeline.clips[3].id);
    assert_eq!(
        findings(&app),
        [
            format!(
                "Clip {d}'s file ends 0.50 s before the transition into clip {e} does; its last picture holds."
            ),
            format!(
                "Clip {e}'s file starts 0.50 s after the transition into it does; its first picture holds."
            ),
        ]
    );
    // With the clip before it gone, the transition has nothing to come from.
    run(&mut app, &format!("deleteClip({d})")).unwrap();
    assert_eq!(
        findings(&app),
        [format!(
            "Clip {e}'s transition has no clip right before it on its lane, so it does not play."
        )]
    );
}

#[test]
fn keyframes_animate_by_script_and_by_the_details_fields() {
    let mut app = Editor::headless();
    app.assets.push(asset("a.mp4", 20.0, true));
    run(&mut app, "addAssetRange('a.mp4', 2, 0, 4);").unwrap();
    let id = app.timeline.clips[0].id;
    // Keys at timeline times on the clip, in setTransform's units.
    run(
        &mut app,
        &format!(
            "setKeyframe({id}, 'scale', 2, 0.5); setKeyframe({id}, 'scale', '4s', 1.5); \
             setKeyframe({id}, 'opacity', 3, 0.2); setKeyframe({id}, 'volume', 'clip:{id}@end', 2);"
        ),
    )
    .unwrap();
    let clip = app.timeline.clip(id).unwrap();
    assert_eq!(clip.transform_at(3.0).scale, 1.0);
    assert_eq!(clip.opacity_at(5.0), 0.2);
    assert_eq!(clip.keyframes.volume[0].at, 4.0);
    let read = run(&mut app, "return clips()[0].keyframes.scale[1]").unwrap();
    assert_eq!(read, r#"{"at":2,"value":1.5}"#);

    // Out of range, off the clip, or a property the clip has not: refused.
    for (code, why) in [
        (
            format!("setKeyframe({id}, 'scale', 3, 0)"),
            "out of range for scale",
        ),
        (
            format!("setKeyframe({id}, 'opacity', 7, 1)"),
            "is not on clip",
        ),
        (
            format!("setKeyframe({id}, 'size', 3, 1)"),
            "unknown property 'size'",
        ),
        (
            format!("removeKeyframe({id}, 'scale', 3)"),
            "no scale key at 3",
        ),
    ] {
        let error = run(&mut app, &code).unwrap_err();
        assert!(error.contains(why), "{code}: {error}");
    }

    // A field edited at the playhead sets the key there once the property
    // has keys; without keys it sets the clip's one value.
    app.seek(3.0);
    app.edit_clip_transform(id, |t| {
        t.scale = 2.0;
        t.y = 0.1;
    });
    let clip = app.timeline.clip(id).unwrap();
    assert_eq!(clip.keyframes.scale.len(), 3);
    assert_eq!(clip.transform_at(3.0).scale, 2.0);
    assert_eq!(clip.transform.y, 0.1);
    assert!(clip.keyframes.y.is_empty());

    // The key button holds the value under the playhead, then takes it away.
    app.toggle_keys(id, &[Property::X, Property::Y]);
    assert_eq!(app.timeline.clip(id).unwrap().keyframes.y[0].value, 0.1);
    assert!(app.keyed_here(id, &[Property::X, Property::Y]));
    app.toggle_keys(id, &[Property::X, Property::Y]);
    assert!(app.timeline.clip(id).unwrap().keyframes.y.is_empty());
    assert_eq!(app.timeline.clip(id).unwrap().transform.y, 0.1);
    assert!(app.seek_key(id, true));
    assert_eq!(app.playhead, 4.0);
    assert!(app.seek_key(id, true));
    assert_eq!(app.playhead, 6.0);
    assert!(!app.seek_key(id, true));

    // A plain setter is one value for the whole clip again; clearing keeps
    // the value at the clip's start.
    run(
        &mut app,
        &format!("setOpacity({id}, 0.6); clearKeyframes({id}, 'scale')"),
    )
    .unwrap();
    let clip = app.timeline.clip(id).unwrap();
    assert!(clip.keyframes.opacity.is_empty() && clip.keyframes.scale.is_empty());
    assert_eq!((clip.opacity, clip.transform.scale), (0.6, 0.5));
    assert_eq!(clip.keyframes.volume.len(), 1);
}

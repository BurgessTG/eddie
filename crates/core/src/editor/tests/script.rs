//! One script as one edit, the path `eddie run` and the live server take:
//! the request-time state, then every command or none. Also the console.

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
        assert!(error.contains("would overlap on lane 0"), "{error}");
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

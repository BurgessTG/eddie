//! Markers: CapCut's keys, undo, saved projects and the script API.

use openreel_engine::MarkerColor;

use super::*;

fn asset(path: &str, duration: f64) -> BinAsset {
    BinAsset {
        info: MediaInfo {
            path: path.to_owned(),
            name: path.to_owned(),
            duration,
            width: 320,
            height: 180,
            fps: 30.0,
            has_video: true,
            has_audio: false,
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
fn keys_add_markers_in_the_colour_last_used_and_jump_to_the_nearest() {
    let mut app = Editor::headless();
    app.timeline
        .add_media_clip("a.mp4", ClipKind::Video, 0, 0.0, 20.0, 0.0, "a.mp4");
    app.commit_history();
    // Added out of time order, so the next marker is the nearest one
    // after the playhead, not the next one added.
    for (time, action) in [
        (12.0, Action::AddMarker),
        (4.0, Action::AddMarkerNewColor),
        (8.0, Action::AddMarker),
    ] {
        app.seek(time);
        app.perform(action);
    }
    let placed: Vec<(f64, MarkerColor)> = app
        .timeline
        .markers_in_order()
        .iter()
        .map(|marker| (marker.time, marker.color))
        .collect();
    assert_eq!(
        placed,
        [
            (4.0, MarkerColor::Orange),
            (8.0, MarkerColor::Orange),
            (12.0, MarkerColor::Yellow)
        ],
        "M keeps the colour Alt+M changed to"
    );
    app.perform(Action::AddMarker);
    assert_eq!(app.timeline.markers.len(), 3, "M on a marker adds none");
    assert_eq!(app.status, "Marker 3 is already here.");

    app.seek(5.0);
    app.perform(Action::NextMarker);
    assert_eq!(app.playhead, 8.0);
    app.perform(Action::NextMarker);
    assert_eq!(app.playhead, 12.0);
    app.perform(Action::NextMarker);
    assert_eq!(
        (app.playhead, app.status.as_str()),
        (12.0, "No marker after the playhead.")
    );
    app.perform(Action::PrevMarker);
    app.perform(Action::PrevMarker);
    assert_eq!(app.playhead, 4.0);

    app.commit_history();
    app.perform(Action::Undo);
    assert!(app.timeline.markers.is_empty(), "undo takes markers back");
}

#[test]
fn markers_are_saved_with_the_project_and_their_ids_are_never_reused() {
    let dir = std::env::temp_dir().join(format!("eddie-markers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let read = |path: &Path| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    };

    // A project without markers is saved as before.
    let plain = dir.join("plain.eddie");
    Editor::headless().save_project_to(&plain).unwrap();
    let timeline = &read(&plain)["timeline"];
    assert!(timeline.get("markers").is_none() && timeline.get("next_marker").is_none());

    let mut app = Editor::headless();
    app.timeline
        .add_marker(1.0, "Logo here", MarkerColor::Purple);
    let last = app.timeline.add_marker(3.0, "", MarkerColor::Yellow);
    app.timeline.remove_marker(last);
    let path = dir.join("marked.eddie");
    app.save_project_to(&path).unwrap();
    assert_eq!(
        read(&path)["timeline"]["markers"],
        serde_json::json!([{"id": 1, "time": 1.0, "name": "Logo here", "color": "purple"}])
    );

    let mut reopened = Editor::headless();
    reopened.open_project(&path).unwrap();
    assert_eq!(reopened.timeline.markers, app.timeline.markers);
    let id = reopened
        .timeline
        .add_marker(2.0, "", MarkerColor::default());
    assert_eq!(id, 3, "marker {last} was deleted, not forgotten");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scripts_add_read_change_and_remove_markers() {
    let mut app = Editor::headless();
    app.assets.push(asset("a.mp4", 20.0));
    run(&mut app, "addAssetRange('a.mp4', 0, 0, 10)").unwrap();

    // Without a colour a marker takes the newest one's, as M does.
    run(
        &mut app,
        "addMarker('clip:1@50%', 'B-roll here', 'red'); addMarker(2.5)",
    )
    .unwrap();
    let listed = run(
        &mut app,
        "return markers().map((m) => [m.id, m.time, m.name, m.color])",
    )
    .unwrap();
    assert_eq!(listed, r#"[[2,2.5,"","red"],[1,5,"B-roll here","red"]]"#);

    // `marker:1` is where marker 1 was when the script started.
    run(
        &mut app,
        "setMarker(1, {time: 7.5, color: 'green'}); addTitle('Look', 'marker:1', 1)",
    )
    .unwrap();
    let marker = app.timeline.marker(1).unwrap();
    assert_eq!(
        (marker.time, marker.color, marker.name.as_str()),
        (7.5, MarkerColor::Green, "B-roll here")
    );
    let title = app
        .timeline
        .clips
        .iter()
        .find(|c| c.name == "Look")
        .unwrap();
    assert_eq!(title.start, 5.0);

    let before = app.timeline.clone();
    for (code, error) in [
        ("addMarker(1, 'x', 'teal')", "use yellow, orange, red"),
        ("removeMarker(9)", "no marker 9"),
        ("setMarker(1, {note: 'x'})", "has no 'note'"),
        ("setMarker(1, {time: -1})", "from 0"),
        ("addTitle('x', 'marker:9', 1)", "markers: 1, 2"),
    ] {
        let got = run(&mut app, code).unwrap_err();
        assert!(got.contains(error), "{code}: {got}");
        assert_eq!(app.timeline, before, "{code} changes nothing");
    }

    // Rebuilding the edit keeps the notes on where things go.
    run(&mut app, "clearTimeline()").unwrap();
    assert!(app.timeline.clips.is_empty());
    assert_eq!(app.timeline.markers.len(), 2);
    run(&mut app, "removeMarker(2)").unwrap();
    assert_eq!(app.timeline.markers.len(), 1);
    run(&mut app, "clearMarkers()").unwrap();
    assert!(app.timeline.markers.is_empty());
}

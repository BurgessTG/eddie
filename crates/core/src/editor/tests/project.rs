//! Imports, saved projects, export results and two editors on one project.

use super::*;

/// A second import requested while one is running must queue
/// (with placeholders visible right away), not be dropped.
#[test]
fn import_while_busy_queues_instead_of_dropping() {
    let mut app = Editor::headless();
    let first = sample_media()[0].clone();
    let second = sample_media()[1].clone();
    app.spawn_import(vec![first]);
    assert!(app.import_rx.is_some());
    app.spawn_import(vec![second.clone()]);
    assert_eq!(app.import_queue, vec![second]);
    assert_eq!(app.assets.len(), 2, "both files show as placeholders");
}

/// A finished import selects itself only when nothing is selected, so
/// a background batch never takes the curator card from the user.
#[test]
fn finished_import_keeps_the_users_selection() {
    let mut app = Editor::headless();
    let music = sample_media()[1].to_str().expect("utf-8 sample path");
    let import = |app: &mut Editor| {
        app.spawn_import(vec![PathBuf::from(music)]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while app.import_rx.is_some() {
            app.poll_imports();
            assert!(std::time::Instant::now() < deadline, "import timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };

    import(&mut app);
    assert_eq!(
        app.selected_asset.as_deref(),
        Some(music),
        "an import fills an empty selection"
    );

    app.assets.retain(|asset| asset.info.path != music);
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/picked.mp4".to_owned(),
            name: "picked.mp4".to_owned(),
            duration: 10.0,
            width: 640,
            height: 360,
            fps: 30.0,
            has_video: true,
            has_audio: false,
        },
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    });
    app.selected_asset = Some("/media/picked.mp4".to_owned());
    import(&mut app);
    assert_eq!(
        app.selected_asset.as_deref(),
        Some("/media/picked.mp4"),
        "a finished import must not replace the user's selection"
    );
}

/// An import decodes a file's sound once: later imports of the unchanged
/// file read the peaks saved beside its curation.
#[test]
fn a_second_import_reads_saved_waveform_peaks() {
    let dir = std::env::temp_dir().join(format!("eddie-saved-peaks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let music = dir.join("music.mp3");
    std::fs::copy(&sample_media()[1], &music).unwrap();
    let path = music.to_string_lossy().into_owned();
    let store = dir.join("store");
    let rate = openreel_media::WAVEFORM_PEAKS_PER_SEC;
    let import = || {
        let mut app = Editor::headless();
        app.curation_dir = Some(store.clone());
        app.spawn_import(vec![music.clone()]);
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while app.import_rx.is_some() {
            app.poll_imports();
            assert!(Instant::now() < deadline, "import timed out");
            thread::sleep(std::time::Duration::from_millis(10));
        }
        app.assets.remove(0)
    };

    let decoded = import();
    assert!(decoded.peaks.iter().any(|&peak| peak > 0.05), "sound");
    assert_eq!(
        sweep::load_peaks(&store, &path, rate).as_ref(),
        Some(&decoded.peaks)
    );

    // Peaks only the store holds show that the import read them.
    let marked: Vec<f32> = decoded.peaks.iter().map(|peak| peak * 0.5).collect();
    sweep::save_peaks(&store, &path, rate, &marked).unwrap();
    let restored = import();
    assert_eq!(restored.peaks, marked);
    assert_eq!(
        restored.beats.is_some(),
        decoded.beats.is_some(),
        "the beat grid is still found from the peaks"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_project_reopens_its_timeline_media_and_fresh_ids() {
    let dir = std::env::temp_dir().join(format!("eddie-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let media = dir.join("shot.mp4");
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=64x36:r=10:d=3",
        ])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
        .arg(&media)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let settle = |app: &mut Editor| {
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        while app.import_rx.is_some() || !app.import_queue.is_empty() {
            app.poll_imports();
            assert!(Instant::now() < deadline, "import timed out");
            thread::sleep(std::time::Duration::from_millis(20));
        }
    };

    let mut app = Editor::headless();
    app.spawn_import(vec![media.clone()]);
    settle(&mut app);
    app.run_console(
        "addAssetRange('shot.mp4', 0.5, 1.25, 1.5); addTitle('Booth', 3, 2);".to_owned(),
    );
    app.mute_music_beds = false;
    assert_eq!(app.timeline.clips.len(), 2, "{}", app.status);
    app.save_project_to(&dir.join("expo")).unwrap();
    assert!(dir.join("expo.eddie").exists(), "the extension is added");
    Editor::headless()
        .save_project_to(&dir.join("cut v1.2"))
        .unwrap();
    assert!(
        dir.join("cut v1.2.eddie").exists(),
        "a dot in the name is not an extension to replace"
    );

    let mut reopened = Editor::headless();
    reopened.open_project(&dir.join("expo.eddie")).unwrap();
    settle(&mut reopened);
    assert_eq!(reopened.timeline, app.timeline);
    assert!(!reopened.mute_music_beds);
    assert_eq!(reopened.assets.len(), 1);
    assert!(reopened.assets[0].state == AssetState::Ready);
    reopened.run_console("addTitle('Next', 6, 1);".to_owned());
    let mut ids: Vec<_> = reopened.timeline.clips.iter().map(|clip| clip.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3, "a new clip never reuses a saved ID");

    assert!(
        !app.has_unsaved_changes(),
        "saving clears the unsaved state"
    );
    assert!(reopened.has_unsaved_changes());

    // Media in use stays in the bin; unused media can be removed.
    let source = reopened.assets[0].info.path.clone();
    reopened.remove_asset(&source);
    assert_eq!(reopened.assets.len(), 1, "{}", reopened.status);
    reopened.timeline.clips.retain(|clip| clip.source.is_none());
    reopened.remove_asset(&source);
    assert!(reopened.assets.is_empty(), "{}", reopened.status);

    // A project whose numbers the editor cannot work with is refused
    // before it replaces anything.
    let saved = std::fs::read_to_string(dir.join("expo.eddie")).unwrap();
    let mut broken: serde_json::Value = serde_json::from_str(&saved).unwrap();
    broken["timeline"]["fps"] = serde_json::json!(0.0);
    std::fs::write(dir.join("broken.eddie"), broken.to_string()).unwrap();
    let kept = reopened.timeline.clone();
    let error = reopened
        .open_project(&dir.join("broken.eddie"))
        .unwrap_err();
    assert_eq!(error, "broken.eddie is not an Eddie project.");
    assert_eq!(reopened.timeline, kept);

    // A new project starts empty.
    reopened.spawn_import(vec![media.clone()]);
    settle(&mut reopened);
    reopened.save_project_to(&dir.join("other")).unwrap();
    reopened.new_project();
    assert!(reopened.timeline.clips.is_empty());
    assert!(reopened.assets.is_empty(), "the bin starts empty");
    assert!(reopened.project_path.is_none(), "Save asks for a new file");
    assert!(!reopened.has_unsaved_changes());
    reopened.mute_music_beds = !reopened.mute_music_beds;
    assert!(
        reopened.has_unsaved_changes(),
        "the music-bed choice is saved with the project"
    );

    // A project named on the command line opens as the project, not as media.
    let mut launched = Editor::headless();
    launched.initial_imports = vec![dir.join("expo.eddie")];
    launched.begin_frame(0.0);
    settle(&mut launched);
    assert_eq!(launched.timeline, app.timeline);
    assert_eq!(
        launched.project_path.as_deref(),
        Some(dir.join("expo.eddie").as_path())
    );
    assert_eq!(launched.assets.len(), 1, "{}", launched.status);

    // The project and its footage move together: nothing goes missing.
    let moved = dir.with_extension("moved");
    let _ = std::fs::remove_dir_all(&moved);
    std::fs::rename(&dir, &moved).unwrap();
    let mut relocated = Editor::headless();
    relocated.open_project(&moved.join("expo.eddie")).unwrap();
    assert_eq!(relocated.status, "Opened expo.eddie.");
    settle(&mut relocated);
    let moved_media = moved.join("shot.mp4").to_string_lossy().into_owned();
    assert_eq!(
        relocated.timeline.clips[0].source.as_deref(),
        Some(moved_media.as_str())
    );
    assert!(relocated.assets[0].info.path == moved_media);
    assert!(
        !relocated.has_unsaved_changes(),
        "an automatic relink is not an edit"
    );

    // Footage moved elsewhere is located by hand.
    let elsewhere = moved.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::rename(moved.join("shot.mp4"), elsewhere.join("shot.mp4")).unwrap();
    let mut missing = Editor::headless();
    missing.open_project(&moved.join("expo.eddie")).unwrap();
    assert_eq!(missing.missing_media.len(), 1);
    assert!(
        missing.status.contains("1 media file is missing"),
        "{}",
        missing.status
    );
    assert_eq!(missing.timeline, app.timeline, "clips stay for relinking");
    missing.run_console("addTitle('Before locating', 8, 1);".to_owned());
    missing.commit_history();
    missing.locate_missing_media(&elsewhere.join("shot.mp4"));
    assert_eq!(missing.status, "Relinked 1 media file.");
    settle(&mut missing);
    assert!(missing.missing_media.is_empty());
    let located = elsewhere.join("shot.mp4").to_string_lossy().into_owned();
    assert_eq!(
        missing.timeline.clips[0].source.as_deref(),
        Some(located.as_str())
    );
    assert!(missing.assets[0].state == AssetState::Ready);
    assert!(missing.has_unsaved_changes(), "the new place needs saving");
    assert!(missing.step_history(false), "the earlier edit still undoes");
    assert_eq!(missing.timeline.clips.len(), 2);
    assert_eq!(
        missing.timeline.clips[0].source.as_deref(),
        Some(located.as_str()),
        "undo does not bring the lost path back"
    );
    assert!(!missing.step_history(false), "locating is not a step");

    // Importing a file again that no longer reads keeps what was known.
    std::fs::write(elsewhere.join("shot.mp4"), "not a video").unwrap();
    missing.spawn_import(vec![elsewhere.join("shot.mp4")]);
    settle(&mut missing);
    assert!(
        missing.assets[0].state == AssetState::Ready,
        "{}",
        missing.status
    );
    assert!(missing.assets[0].info.duration > 0.0);
    assert!(missing.import_errors.contains_key(&located));

    let before = missing.timeline.clone();
    std::fs::write(moved.join("bad.eddie"), "{}").unwrap();
    assert!(missing.open_project(&moved.join("bad.eddie")).is_err());
    assert_eq!(missing.timeline, before, "a bad file changes nothing");
    let _ = std::fs::remove_dir_all(&moved);
}

#[test]
fn a_finished_export_reports_itself_and_where_it_went() {
    let mut app = Editor::headless();
    let (tx, rx) = channel();
    app.export_rx = Some(rx);
    let path = PathBuf::from("/videos/cut/montage.mp4");
    tx.send(ExportMsg::Done(Ok(path.clone()))).unwrap();
    assert!(app.poll_export(), "a successful export reports itself");
    assert_eq!(app.status, "Exported montage.mp4");
    assert_eq!(app.exported, Some((path, app.status.clone())));
}

/// A second window is still told why agents do not reach it once its
/// media has loaded: every finished import writes the status.
#[cfg(unix)]
#[test]
fn a_second_window_says_so_after_its_media_loads() {
    let dir = std::env::temp_dir().join(format!("eddie-second-window-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let project = dir.join("p.eddie");
    let sockets = dir.join("run");
    let settle = |app: &mut Editor| {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            app.begin_frame(0.0);
            app.end_frame(false, false);
            if !app.importing() {
                break;
            }
            assert!(Instant::now() < deadline, "import timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let window = |project: Option<&std::path::Path>| {
        let mut app = Editor::headless();
        app.listen = true;
        app.socket_dir = Some(sockets.clone());
        if let Some(project) = project {
            app.open_project(project).unwrap();
        }
        app
    };

    let mut first = window(None);
    first.spawn_import(vec![sample_media()[1].clone()]);
    settle(&mut first);
    first.save_project_to(&project).unwrap();
    settle(&mut first);
    assert!(first.server.is_some(), "{}", first.status);

    let mut second = window(Some(&project));
    assert!(
        second.importing(),
        "the project's media loads in the background"
    );
    settle(&mut second);
    assert!(second.server.is_none());
    assert_eq!(second.status, "Another Eddie already has p.eddie open.");

    // It is said once: later status lines stay.
    second.status = "Split.".to_owned();
    second.listen_refused = second
        .listen_refused
        .take()
        .map(|(project, at)| (project, at - Duration::from_secs(2)));
    settle(&mut second);
    assert_eq!(second.status, "Split.");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Frames until a window listens for its open project, asking again at
/// once instead of after the two seconds a window waits. In a test run the
/// first ask can be refused although the last editor is gone: a child
/// process another test starts holds a copy of that editor's file lock
/// from its fork until its exec.
#[cfg(unix)]
fn frames_until_listening(window: &mut Editor) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        window.listen_refused = window
            .listen_refused
            .take()
            .map(|(project, at)| (project, at - Duration::from_secs(2)));
        window.begin_frame(0.0);
        window.end_frame(false, false);
        if window.server.is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "{}", window.status);
        thread::sleep(Duration::from_millis(5));
    }
}

/// One editor listens per project. A second `eddie serve` is refused; a
/// window that opens the project takes it over from the server, and only
/// its user can stop it.
#[cfg(unix)]
#[test]
fn a_window_takes_a_served_project_over() {
    use std::sync::mpsc::sync_channel;

    let dir = std::env::temp_dir().join(format!("eddie-take-over-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let project = dir.join("p.eddie");
    Editor::headless().save_project_to(&project).unwrap();
    let sockets = dir.join("run");

    let (listening, ready) = channel();
    let served = {
        let (project, sockets) = (project.clone(), sockets.clone());
        thread::spawn(move || {
            let (wake, woken) = sync_channel(1);
            let wake = Wake::new(move || {
                let _ = wake.try_send(());
            });
            let mut app = Editor::headless_with(wake, Box::new(NoPreview));
            app.socket_dir = Some(sockets);
            app.open_project(&project).unwrap();
            app.serve().unwrap();
            listening.send(()).unwrap();
            let started = Instant::now();
            while !app.serve_stopped() {
                let _ = woken.recv_timeout(Duration::from_millis(200));
                app.begin_frame(started.elapsed().as_secs_f64());
                app.end_frame(false, false);
                assert!(started.elapsed() < Duration::from_secs(30), "never stopped");
            }
        })
    };
    ready.recv().unwrap();

    let mut second = Editor::headless();
    second.socket_dir = Some(sockets.clone());
    second.open_project(&project).unwrap();
    let refused = second.serve().unwrap_err();
    assert_eq!(refused, "Another Eddie already has p.eddie open.");

    let mut window = Editor::headless();
    window.listen = true;
    window.socket_dir = Some(sockets.clone());
    window.open_project(&project).unwrap();
    served.join().unwrap();
    frames_until_listening(&mut window);

    let ask = |method: &'static str, params: serde_json::Value| {
        let (project, sockets) = (project.clone(), sockets.clone());
        thread::spawn(move || {
            let mut live = server::connect_in(&sockets, &project).expect("the window listens");
            live.request(method, &params, &mut |_| {})
        })
    };
    let frames = |window: &mut Editor, pointer_down: bool, done: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done() {
            window.begin_frame(0.0);
            window.end_frame(pointer_down, false);
            thread::sleep(Duration::from_millis(5));
            assert!(Instant::now() < deadline, "the window never answered");
        }
    };

    // An edit waits while the user is mid-drag; a question does not.
    window.end_frame(true, false);
    let question = ask("inspect", serde_json::json!({}));
    frames(&mut window, true, &|| question.is_finished());
    assert_eq!(question.join().unwrap().unwrap()["ok"], true);
    let edit = ask("run", serde_json::json!({ "code": "addTitle('Hi', 0, 1)" }));
    let waited = Instant::now();
    frames(&mut window, true, &|| {
        waited.elapsed() > Duration::from_millis(150)
    });
    assert!(!edit.is_finished() && window.timeline.clips.is_empty());
    frames(&mut window, false, &|| edit.is_finished());
    let edited = edit.join().unwrap().unwrap();
    assert_eq!(
        (&edited["ok"], &edited["changed"]),
        (&true.into(), &true.into())
    );
    assert_eq!(window.timeline.clips.len(), 1);
    assert_eq!(window.status, "Edited from the command line.");
    // The window's file is its user's to save, and the edit theirs to undo.
    assert_eq!(edited["saved"], false, "{edited}");
    assert!(window.step_history(false));
    assert!(window.timeline.clips.is_empty());

    let asked = ask("stop", serde_json::json!({}));
    frames(&mut window, false, &|| asked.is_finished());
    let answer = asked.join().unwrap().unwrap();
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap()
            .contains("close the window"),
        "{answer}"
    );
    assert!(window.server.is_some());

    // Reopening its own project never lets go of the file. A child
    // process started at that moment would keep a copy of the hold
    // until it execs, and the window could not take the file back.
    let copy = window.project_lock.as_ref().unwrap().try_clone().unwrap();
    window.open_project(&project).unwrap();
    assert!(window.project_lock.is_some(), "the window let go");
    drop(copy);

    // A second window opens the project but is told agents reach the
    // other one. It takes the socket once that closes.
    let mut late = Editor::headless();
    late.listen = true;
    late.socket_dir = Some(sockets.clone());
    late.open_project(&project).unwrap();
    late.begin_frame(0.0);
    late.end_frame(false, false);
    assert!(late.server.is_none());
    assert_eq!(late.status, "Another Eddie already has p.eddie open.");
    assert!(late.repaint_after().is_some(), "it asks again unprompted");

    // The socket goes with the editor.
    drop(window);
    assert!(server::connect_in(&sockets, &project).is_none());
    frames_until_listening(&mut late);
    let _ = std::fs::remove_dir_all(&dir);
}

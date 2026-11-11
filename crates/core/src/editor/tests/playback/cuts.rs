//! Playing across cuts: prerolled pictures, parked decoders and the audio
//! clock that waits for them.

use super::*;

#[test]
fn preview_boundary_never_uploads_the_previous_clip() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    for (start, source_in) in [(0.0, 0.0), (0.2, 1.0)] {
        app.timeline.add_media_clip(
            "Short excerpt",
            ClipKind::Video,
            0,
            start,
            0.2,
            source_in,
            &fixture.path,
        );
    }
    app.set_playing(true);
    let first = await_picture(&mut app, &monitor);
    assert!(first.r() > 240 && first.b() < 10, "first frame: {first:?}");

    // This is the production order: advance the playhead, poll frames,
    // paint panels, and only then synchronize the changed source.
    app.playhead = 0.2;
    app.poll_frames();
    assert!(
        !monitor.on(),
        "old clip texture must not be painted under the new clip's label"
    );
    let next = await_picture(&mut app, &monitor);
    assert!(next.b() > 240 && next.r() < 10, "next frame: {next:?}");
    app.set_playing(false);
}

#[test]
fn preview_transport_waits_for_each_short_clips_first_picture() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    for (start, source_in) in [(0.0, 0.0), (0.05, 1.0)] {
        app.timeline.add_media_clip(
            "Short excerpt",
            ClipKind::Video,
            0,
            start,
            0.05,
            source_in,
            &fixture.path,
        );
    }
    app.last_now = Some(0.0);
    app.set_playing(true);
    app.sync_playback();
    app.advance_playback(1.0);
    assert_eq!(
        app.playhead, 0.0,
        "wall time must not consume a clip before its picture is uploaded"
    );
    assert!(app.playing);
    let first = await_picture(&mut app, &monitor);
    assert!(first.r() > 240 && first.b() < 10);

    // Even a late UI tick must land at the next clip's entry, not
    // jump over an entire excerpt that is shorter than the tick.
    app.advance_playback(1.5);
    assert_eq!(app.playhead, 0.05);
    assert!(app.playing);
    app.poll_frames();
    assert!(!monitor.on());
    app.sync_playback();
    app.advance_playback(2.0);
    assert_eq!(app.playhead, 0.05);
    let next = await_picture(&mut app, &monitor);
    assert!(next.b() > 240 && next.r() < 10);
    app.advance_playback(2.5);
    assert_eq!(app.playhead, 0.1);
    assert!(!app.playing);
}

#[test]
fn preview_prerolls_the_next_cut_before_the_playhead_reaches_it() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    // Red is source [0, 1), blue [1, 2), with one keyframe at 0. The
    // backward cuts need the idle lane's preroll; the forward cut
    // continues on the active decoder instead of reseeking. Cutting to
    // 1.6 from a clip that plays 1.5 to 1.7 is backward too: it is after
    // that clip's in point but before where its decoder will be.
    let cuts = [
        (1.0, true),
        (0.0, false),
        (1.5, true),
        (1.6, true),
        (0.5, false),
    ];
    for (source_in, _) in cuts {
        let start = app.timeline.end();
        app.timeline.add_media_clip(
            "Cut",
            ClipKind::Video,
            0,
            start,
            0.2,
            source_in,
            &fixture.path,
        );
    }
    app.last_now = Some(0.0);
    app.set_playing(true);
    for (index, (source_in, blue)) in cuts.into_iter().enumerate() {
        let picture = await_picture(&mut app, &monitor);
        assert_eq!(
            picture.b() > 240 && picture.r() < 10,
            blue,
            "cut {index}: {picture:?}"
        );
        let Some((_, Some((source, time)))) = app.next_video_change(app.playhead) else {
            break;
        };
        let source = source.to_owned();
        if time < source_in + 0.2 {
            let stream = app.stream.as_ref().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !stream.prefetch_ready(&source, time, PREVIEW_W) {
                assert!(
                    Instant::now() < deadline,
                    "cut {} was not prerolled before the playhead reached it",
                    index + 1
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        // A late tick lands exactly on the cut that was prerolled.
        app.advance_playback(index as f64 + 1.0);
        assert_eq!(app.playhead, app.timeline.clips[index + 1].start);
    }
    assert_eq!(
        app.stream.as_ref().unwrap().prefetch_handoffs(),
        3,
        "every backward cut must be served from its prerolled picture"
    );
    app.set_playing(false);
}

/// After a cut the lane that played the previous file is idle, and with
/// no later cut it has nothing to preroll. Pausing drops its decoder
/// all the same, so a paused editor holds one.
#[test]
fn pausing_after_a_cut_keeps_one_preview_decoder() {
    let fixture = Fixture::new();
    let other = fixture.dir.join("b.mp4");
    std::fs::copy(&fixture.path, &other).unwrap();
    let other = other.to_string_lossy().into_owned();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    add_asset(&mut app, &other);
    for (start, path) in [(0.0, &fixture.path), (0.2, &other)] {
        app.timeline
            .add_media_clip("Cut", ClipKind::Video, 0, start, 0.2, 0.0, path);
    }
    // No parking, as on machines with eight or more cores.
    let stream = FrameStream::spawn_preview(|| {}).unwrap();
    stream.set_parked_decoders(0);
    app.stream = Some(stream);
    app.last_now = Some(0.0);
    app.set_playing(true);
    await_picture(&mut app, &monitor);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app
        .stream
        .as_ref()
        .unwrap()
        .prefetch_ready(&other, 0.0, PREVIEW_W)
    {
        assert!(Instant::now() < deadline, "second cut was not prerolled");
        thread::sleep(Duration::from_millis(5));
    }
    app.advance_playback(0.3);
    assert_eq!(app.playhead, 0.2);
    await_picture(&mut app, &monitor);
    let decoders = |app: &Editor| app.stream.as_ref().unwrap().preview_decoders();
    assert_eq!(
        decoders(&app),
        2,
        "one per file while both lanes are in use"
    );

    app.set_playing(false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while decoders(&app) != 1 {
        assert!(
            Instant::now() < deadline,
            "paused with {} decoders open",
            decoders(&app)
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn preview_reentering_a_file_reuses_its_parked_decoder() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    let mut paths = vec![fixture.path.clone()];
    for name in ["b.mp4", "c.mp4"] {
        let copy = fixture.dir.join(name);
        std::fs::copy(&fixture.path, &copy).unwrap();
        paths.push(copy.to_string_lossy().into_owned());
    }
    for path in &paths {
        add_asset(&mut app, path);
    }
    // A, B, C, then back to A. Both lanes have moved on from A by
    // then, so only a parked decoder avoids opening it again.
    let cuts = [
        (0, 0.0, false),
        (1, 0.0, false),
        (2, 0.0, false),
        (0, 1.2, true),
    ];
    for (file, source_in, _) in cuts {
        let start = app.timeline.end();
        app.timeline.add_media_clip(
            "Cut",
            ClipKind::Video,
            0,
            start,
            0.2,
            source_in,
            &paths[file],
        );
    }
    // Parking is off on 8+ cores; this tests the mechanism itself.
    let stream = FrameStream::spawn_preview(|| {}).unwrap();
    stream.set_parked_decoders(4);
    app.stream = Some(stream);
    app.last_now = Some(0.0);
    app.set_playing(true);
    for (index, (_, _, blue)) in cuts.into_iter().enumerate() {
        let picture = await_picture(&mut app, &monitor);
        assert_eq!(
            picture.b() > 240 && picture.r() < 10,
            blue,
            "cut {index}: {picture:?}"
        );
        let Some((_, Some((source, time)))) = app.next_video_change(app.playhead) else {
            break;
        };
        let source = source.to_owned();
        let stream = app.stream.as_ref().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stream.prefetch_ready(&source, time, PREVIEW_W) {
            assert!(Instant::now() < deadline, "cut {} not prerolled", index + 1);
            thread::sleep(Duration::from_millis(5));
        }
        app.advance_playback(index as f64 + 1.0);
        assert_eq!(app.playhead, app.timeline.clips[index + 1].start);
    }
    assert_eq!(
        app.stream.as_ref().unwrap().preview_opens(),
        3,
        "re-entering A must reuse its parked decoder"
    );
    app.set_playing(false);
}

#[test]
fn preview_prerolled_cut_keeps_the_audio_clock_running() {
    let fixture = Fixture::new();
    let other = fixture.dir.join("b.mp4");
    std::fs::copy(&fixture.path, &other).unwrap();
    let other = other.to_string_lossy().into_owned();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    add_asset(&mut app, &other);
    for (start, path) in [(0.0, &fixture.path), (0.2, &other)] {
        app.timeline
            .add_media_clip("Cut", ClipKind::Video, 0, start, 0.2, 0.0, path);
    }
    app.last_now = Some(0.0);
    app.set_playing(true);
    await_picture(&mut app, &monitor);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app
        .stream
        .as_ref()
        .unwrap()
        .prefetch_ready(&other, 0.0, PREVIEW_W)
    {
        assert!(Instant::now() < deadline, "second cut was not prerolled");
        thread::sleep(Duration::from_millis(5));
    }
    app.advance_playback(0.3);
    assert_eq!(app.playhead, 0.2);
    app.sync_playback();
    // The prerolled picture is handed over, not decoded: the audio
    // clock must not stall a device block waiting for its upload.
    assert!(
        !app.waiting_for_video(),
        "prerolled cut treated as buffering"
    );
    // Audio rendered on through the cut; the playhead rejoins it.
    app.advance_playback(0.35);
    assert!((app.playhead - 0.35).abs() < 1e-9, "{}", app.playhead);
    app.set_playing(false);
}

#[test]
fn preview_buffers_audio_with_video_but_keeps_audio_only_gaps_moving() {
    let fixture = Fixture::new();
    let path = fixture.dir.join("with-audio.mp4");
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i", &fixture.path])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-threads:a",
            "1",
            "-shortest",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let path = path.to_str().unwrap();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, path);
    let video = app
        .timeline
        .add_media_clip("Video", ClipKind::Video, 0, 0.2, 0.2, 0.0, path);
    let bed = app
        .timeline
        .add_media_clip("Bed", ClipKind::Audio, 1, 0.0, 1.0, 0.0, path);
    app.last_now = Some(0.0);
    app.set_playing(true);
    app.advance_playback(0.1);
    assert_eq!(app.playhead, 0.1);
    app.sync_playback();
    // The look-ahead starts the video's audio before its edge.
    let voices = |app: &Editor| app.audio.as_ref().unwrap().voice_ids();
    assert_eq!(voices(&app), vec![video, bed]);

    app.advance_playback(0.3);
    assert_eq!(app.playhead, 0.2);
    app.poll_frames();
    app.sync_playback();
    app.advance_playback(0.5);
    assert_eq!(app.playhead, 0.2);
    assert_eq!(
        app.audio.as_ref().unwrap().clock(),
        0.2,
        "audio must not run ahead during preroll"
    );
    let picture = await_picture(&mut app, &monitor);
    assert!(picture.r() > 240 && picture.b() < 10);
    assert_eq!(voices(&app), vec![video, bed]);

    // The picture ends at 0.4; the audio-only gap keeps the clock.
    app.advance_playback(0.8);
    assert!((app.playhead - 0.5).abs() < 1e-9, "{}", app.playhead);
    app.poll_frames();
    app.sync_playback();
    assert_eq!(voices(&app), vec![bed]);
    app.advance_playback(0.9);
    assert!((app.playhead - 0.6).abs() < 1e-9, "{}", app.playhead);
    app.set_playing(false);
}

/// Diagnostic against reporter-supplied files, not a native-window or
/// export test. Enable explicitly; fixtures are not committed.
#[test]
#[ignore = "set OPENREEL_PREVIEW_MEDIA_DIR to the supplied excerpt directory"]
fn preview_short_clip_diagnostics() {
    run_short_clip_diagnostics(false);
}

#[test]
#[ignore = "set OPENREEL_PREVIEW_MEDIA_DIR to the supplied excerpt directory"]
fn preview_short_clip_transport_diagnostics() {
    run_short_clip_diagnostics(true);
}

fn run_short_clip_diagnostics(transport_clock: bool) {
    let dir = std::env::var("OPENREEL_PREVIEW_MEDIA_DIR").expect("excerpt directory");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "mp4"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    let (mut app, monitor) = watched_editor();
    for path in &paths {
        add_asset(&mut app, path.to_str().unwrap());
    }
    for pass in 0..2 {
        for path in &paths {
            let id = app.timeline.add_media_clip(
                path.file_name().unwrap().to_str().unwrap(),
                ClipKind::Video,
                0,
                app.timeline.end(),
                0.3,
                1.0 + pass as f64,
                path.to_str().unwrap(),
            );
            app.timeline.clip_mut(id).unwrap().embedded_audio = false;
        }
    }
    let mut sync_ms = Vec::new();
    let mut uploads: HashMap<ClipId, usize> = HashMap::new();
    let mut first_uploads: HashMap<ClipId, f64> = HashMap::new();
    let mut transitions = Vec::new();
    let mut previous = None;
    let mut buffered_seconds = 0.0;
    let mut previous_wall = 0.0;
    app.set_playing(true);
    let started = Instant::now();
    loop {
        let wall = started.elapsed().as_secs_f64();
        if transport_clock {
            assert!(wall < 30.0, "preview did not finish: {}", app.status);
            if app.waiting_for_video() {
                buffered_seconds += wall - previous_wall;
            }
            app.advance_playback(wall);
            if !app.playing {
                assert_eq!(app.playhead, app.timeline.end(), "{}", app.status);
                break;
            }
        } else {
            if wall >= app.timeline.end() {
                break;
            }
            app.playhead = wall;
            app.last_now = Some(app.playhead);
        }
        previous_wall = wall;
        let active = app.active_video_clip().unwrap();
        app.poll_frames();
        let syncing = Instant::now();
        app.sync_playback();
        let elapsed = syncing.elapsed().as_secs_f64() * 1000.0;
        sync_ms.push(elapsed);
        if previous != Some(active.id) {
            transitions.push((active.id, app.playhead, elapsed));
            previous = Some(active.id);
        }
        if !monitor.take().is_empty() {
            *uploads.entry(active.id).or_default() += 1;
            first_uploads.entry(active.id).or_insert(wall);
        }
        thread::sleep(Duration::from_millis(8));
    }
    app.set_playing(false);
    sync_ms.sort_by(f64::total_cmp);
    eprintln!("preview short-clip diagnostics: elapsed={:?}, sync samples={}, p50_ms={:.3}, p95_ms={:.3}, max_ms={:.3}",
            started.elapsed(), sync_ms.len(), sync_ms[sync_ms.len() / 2],
            sync_ms[sync_ms.len() * 95 / 100], sync_ms.last().unwrap());
    eprintln!("transport_clock={transport_clock}, buffered_seconds={buffered_seconds:.3}");
    eprintln!("transitions (clip, timeline seconds, sync ms): {transitions:?}");
    for clip in &app.timeline.clips {
        let count = uploads.get(&clip.id).copied().unwrap_or(0);
        eprintln!(
            "{} {} source={}+{} timeline={} uploads_while_active={} first_upload_wall={:?}",
            clip.id,
            clip.name,
            clip.source_in,
            clip.duration,
            clip.start,
            count,
            first_uploads.get(&clip.id),
        );
        if transport_clock {
            assert!(count >= 2, "{} did not deliver moving pictures", clip.name);
        }
    }
}

//! Paused pictures: scrubbing, arrow steps and stills that arrive late.

use super::*;

/// A drag that moves to a new frame every UI frame, faster than the
/// still decoder answers, still shows pictures while it moves.
#[test]
fn continuous_scrub_shows_pictures_while_the_pointer_moves() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    app.timeline
        .add_media_clip("Scrub", ClipKind::Video, 0, 0.0, 2.0, 0.0, &fixture.path);
    app.transport_scrubbing = true;
    let deadline = Instant::now() + Duration::from_secs(5);
    for step in 1_u32.. {
        app.playhead = f64::from(step % 20) / 10.0;
        app.poll_frames();
        app.sync_playback();
        if monitor.on() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no picture during a continuous scrub"
        );
        thread::sleep(Duration::from_micros(200));
    }
}

/// Paused on a picture, the frames an arrow step reaches are decoded
/// ahead, so stepping across a cut in the source shows the new
/// picture in the same UI frame, without waiting for the decoder.
#[test]
fn paused_arrow_steps_show_prefetched_neighbors_at_once() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    app.timeline
        .add_media_clip("Steps", ClipKind::Video, 0, 0.0, 2.0, 0.0, &fixture.path);
    let frame = 1.0 / app.timeline.fps;
    // Red is source [0, 1), blue [1, 2): the next frame is blue.
    app.seek(1.0 - frame);
    let red = await_picture(&mut app, &monitor);
    assert!(red.r() > 240 && red.b() < 10, "paused picture: {red:?}");
    let next = app.still_target(app.timeline.snap(1.0), PREVIEW_W).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.still_cache.iter().any(|(key, _)| *key == next.key) {
        assert!(Instant::now() < deadline, "next frame was not prefetched");
        app.poll_frames();
        app.sync_playback();
        thread::sleep(Duration::from_millis(5));
    }
    monitor.take();
    for (time, blue) in [(1.0, true), (1.0 - frame, false)] {
        app.seek(time);
        app.poll_frames();
        app.sync_playback();
        let picture = monitor
            .take()
            .pop()
            .unwrap_or_else(|| panic!("step to {time} waited for the decoder"));
        assert_eq!(
            picture.b() > 240 && picture.r() < 10,
            blue,
            "step to {time}: {picture:?}"
        );
    }
}

#[test]
fn a_still_that_cannot_decode_is_not_asked_for_every_frame() {
    let mut app = Editor::headless();
    let gone = "/nonexistent/eddie-gone.mp4";
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: gone.to_owned(),
            name: "gone.mp4".to_owned(),
            duration: 10.0,
            width: 320,
            height: 180,
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
    app.timeline
        .add_media_clip("gone", ClipKind::Video, 0, 0.0, 5.0, 0.0, gone);
    let until = Instant::now() + Duration::from_millis(400);
    while Instant::now() < until {
        app.poll_frames();
        app.sync_playback();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.still_inflight.is_none(), "the request was answered");
    assert_eq!(app.still_gen, 1, "one request, then a wait");
}

#[test]
fn preview_late_still_cannot_overwrite_playback_after_a_seek() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    app.timeline
        .add_media_clip("Picture", ClipKind::Video, 0, 0.0, 2.0, 0.0, &fixture.path);
    app.request_still_if_needed();
    // Hold a real native still result until after playback has changed
    // the picture. Only delivery timing is controlled, not decoding.
    let delayed = app.still_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(delayed.1.is_ok());
    let (tx, rx) = channel();
    app.still_rx = rx;
    app.seek(1.0);
    app.set_playing(true);
    let picture = await_picture(&mut app, &monitor);
    assert!(picture.b() > 240 && picture.r() < 10);
    tx.send(delayed).unwrap();
    app.poll_frames();
    assert!(
        monitor.take().is_empty(),
        "the delayed red still must not replace the blue playback picture"
    );
    app.set_playing(false);
}

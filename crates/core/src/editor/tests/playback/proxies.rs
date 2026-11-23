//! Proxies: with Settings → Proxies on, the preview plays and steps
//! through a light copy of footage wider than it, made in the background,
//! and goes back to the file when they are turned off.

use super::*;

/// Two seconds of `colour` at `size` and 10 fps, written to `path`.
fn footage(path: &Path, colour: &str, size: &str) {
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color={colour}:s={size}:r=10:d=2"))
        .args(["-c:v", "libx264", "-threads", "1"])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
}

/// Wait until the monitor shows a picture `wanted` accepts.
fn await_shown(app: &mut Editor, monitor: &Monitor, wanted: impl Fn(Rgb) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = None;
    loop {
        app.poll_frames();
        app.sync_playback();
        let shown = monitor.take();
        if shown.iter().any(|&picture| wanted(picture)) {
            return;
        }
        last = shown.last().copied().or(last);
        assert!(Instant::now() < deadline, "{what}: last picture {last:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_preview_plays_wide_footage_from_its_proxy_while_proxies_are_on() {
    let fixture = Fixture::new();
    let wide = fixture.dir.join("wide.mp4");
    footage(&wide, "red", "1280x720");
    let wide = wide.to_string_lossy().into_owned();
    let narrow = fixture.dir.join("narrow.mp4");
    footage(&narrow, "red", "640x360");
    let narrow = narrow.to_string_lossy().into_owned();
    let (mut app, monitor) = watched_editor();
    app.proxy_dir = Some(fixture.dir.join("proxies"));
    app.settings.proxies = true;
    add_asset(&mut app, &narrow);
    add_asset(&mut app, &wide);
    app.timeline
        .add_media_clip("Wide", ClipKind::Video, 0, 0.0, 2.0, 0.0, &wide);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !app.has_proxy(&wide) {
        app.begin_frame(0.0);
        assert!(
            Instant::now() < deadline,
            "no proxy: {:?}",
            app.proxy_progress()
        );
        thread::sleep(Duration::from_millis(10));
    }
    for _ in 0..5 {
        app.begin_frame(0.0);
    }
    assert!(
        !app.has_proxy(&narrow) && app.proxy_progress().is_none(),
        "footage no wider than the preview plays from its file"
    );
    // The copy turned green, so a picture decoded from it tells.
    let proxy = app.preview_file(&wide).to_owned();
    assert_ne!(proxy, wide);
    footage(Path::new(&proxy), "lime", "960x540");

    app.playhead = 0.5;
    await_shown(&mut app, &monitor, |p| p.r() < 60, "paused on the proxy");
    app.set_playing(true);
    await_shown(&mut app, &monitor, |p| p.r() < 60, "playing the proxy");
    app.set_playing(false);

    app.settings.proxies = false;
    app.begin_frame(0.0);
    await_shown(&mut app, &monitor, |p| p.r() > 200, "proxies off");
    app.set_playing(true);
    await_shown(&mut app, &monitor, |p| p.r() > 200, "playing the file");
    app.set_playing(false);
}

//! The transport, the program picture and the preview's loudness, against
//! real decoders on generated media.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::*;

mod cuts;
mod loudness;
mod stills;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    dir: PathBuf,
    path: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "eddie-preview-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("red-blue.mp4").to_string_lossy().into_owned();
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=64x36:r=10:d=1",
                "-f",
                "lavfi",
                "-i",
                "color=blue:s=64x36:r=10:d=1",
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-g",
                "100",
                "-bf",
                "3",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        Self { dir, path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn add_asset(app: &mut Editor, path: &str) {
    app.assets.push(BinAsset {
        info: openreel_media::probe(path).expect("probe preview fixture"),
        size_bytes: 0,
        state: AssetState::Ready,
        peaks: Vec::new(),
        beats: None,
        thumbs: Vec::new(),
        sweep: sweep::SweepData::default(),
    });
}

fn await_picture(app: &mut Editor, monitor: &Monitor) -> Rgb {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_frames();
        app.sync_playback();
        if let Some(picture) = monitor.take().pop() {
            return picture;
        }
        assert!(
            Instant::now() < deadline,
            "picture timed out: {}",
            app.status
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// The program monitor of an editor under test: whether a picture is on
/// it and the centre pixel of each one shown since the last `take`.
#[derive(Clone, Default)]
struct Monitor(Rc<RefCell<(bool, Vec<Rgb>)>>);

#[derive(Clone, Copy, Debug)]
struct Rgb([u8; 3]);

impl Rgb {
    fn r(self) -> u8 {
        self.0[0]
    }

    fn b(self) -> u8 {
        self.0[2]
    }
}

impl Monitor {
    fn on(&self) -> bool {
        self.0.borrow().0
    }

    fn take(&self) -> Vec<Rgb> {
        std::mem::take(&mut self.0.borrow_mut().1)
    }
}

impl PreviewSink for Monitor {
    fn show(&mut self, frame: &RawFrame) {
        let centre = frame.rgba.len() / 8 * 4;
        let pixel = &frame.rgba[centre..centre + 3];
        let mut monitor = self.0.borrow_mut();
        monitor.0 = true;
        monitor.1.push(Rgb([pixel[0], pixel[1], pixel[2]]));
    }

    fn clear(&mut self) {
        self.0.borrow_mut().0 = false;
    }
}

fn watched_editor() -> (Editor, Monitor) {
    let monitor = Monitor::default();
    let editor = Editor::headless_with(Wake::none(), Box::new(monitor.clone()));
    (editor, monitor)
}

/// An unplugged device stops its engine's clock for good. The editor
/// opens a new engine and playback goes on from the same place.
#[test]
fn a_lost_audio_device_is_replaced_and_playback_continues() {
    let fixture = Fixture::new();
    let tone = fixture.dir.join("tone.wav");
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=48000:d=3")
        .arg(&tone)
        .output()
        .unwrap();
    assert!(output.status.success());
    let tone = tone.to_string_lossy().into_owned();
    let mut app = Editor::headless();
    add_asset(&mut app, &tone);
    let clip = app
        .timeline
        .add_media_clip("tone", ClipKind::Audio, 0, 0.0, 3.0, 0.0, &tone);
    app.last_now = Some(0.0);
    app.set_playing(true);
    app.sync_playback();
    app.advance_playback(0.1);
    assert!((app.playhead - 0.1).abs() < 1e-9, "{}", app.playhead);

    app.audio.as_ref().unwrap().lose_device();
    app.sync_playback();
    let audio = app.audio.as_ref().unwrap();
    assert!(!audio.device_lost(), "the lost engine is still in use");
    assert_eq!(audio.voice_ids(), vec![clip], "the clip plays on");
    assert_eq!(app.status, "Audio device lost; switched output.");
    app.advance_playback(0.2);
    assert!((app.playhead - 0.2).abs() < 1e-9, "{}", app.playhead);
    assert!(app.playing);
    app.set_playing(false);
}

/// A dry run puts back everything it changed, so what a playing editor
/// has open plays on through it. The same edit for real starts again.
#[test]
fn a_dry_run_leaves_playback_running() {
    use commands::Reply;
    let fixture = Fixture::new();
    let tone = fixture.dir.join("tone.wav");
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:sample_rate=48000:d=3")
        .arg(&tone)
        .output()
        .unwrap();
    assert!(output.status.success());
    let tone = tone.to_string_lossy().into_owned();
    let mut app = Editor::headless();
    add_asset(&mut app, &tone);
    let clip = app
        .timeline
        .add_media_clip("tone", ClipKind::Audio, 0, 0.0, 3.0, 0.0, &tone);
    app.last_now = Some(0.0);
    app.set_playing(true);
    app.sync_playback();
    app.advance_playback(0.1);
    app.sync_playback();
    let voices = |app: &Editor| app.audio.as_ref().unwrap().voice_ids();
    assert_eq!(voices(&app), vec![clip]);

    let run = |app: &mut Editor, code: &str, dry_run: bool| {
        let params = serde_json::json!({ "code": code, "dryRun": dry_run });
        match commands::start(app, "run", &params) {
            Ok(Reply::Done(outcome)) => Ok(outcome.value),
            Ok(Reply::Later(_)) => panic!("{code} waited"),
            Err(error) => Err(error),
        }
    };
    let edit = "addTitle('Opening', 1, 1); seek(2); pause()";
    let dry = run(&mut app, edit, true).unwrap();
    assert_eq!(dry["changed"], true);
    assert_eq!(dry["clips"].as_array().unwrap().len(), 2);
    assert_eq!(app.timeline.clips.len(), 1);
    assert!(app.playing);
    assert_eq!(voices(&app), vec![clip], "the sound started again");
    app.advance_playback(0.2);
    assert!((app.playhead - 0.2).abs() < 1e-9, "{}", app.playhead);

    // A dry run that fails part way is put back the same way.
    let refused = run(&mut app, "seek(2); deleteClip(999)", true);
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(voices(&app), vec![clip], "the sound started again");
    assert!((app.playhead - 0.2).abs() < 1e-9, "{}", app.playhead);

    run(&mut app, edit, false).unwrap();
    assert_eq!(app.timeline.clips.len(), 2);
    assert!(voices(&app).is_empty(), "a real edit starts sources again");
    assert!(!app.playing);
}

#[test]
fn preview_source_remap_restarts_the_same_clip_and_invalidates_its_picture() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    let clip =
        app.timeline
            .add_media_clip("Excerpt", ClipKind::Video, 0, 0.0, 0.5, 0.0, &fixture.path);
    app.set_playing(true);
    let first = await_picture(&mut app, &monitor);
    assert!(first.r() > 240 && first.b() < 10);
    assert!(app.preview_is_current());
    app.timeline.clip_mut(clip).unwrap().source_in = 1.0;
    // Details/console can change the source after poll_frames ran but
    // before the front end paints Program in the same frame.
    assert!(
        !app.preview_is_current(),
        "Program must not paint the old source mapping"
    );
    app.poll_frames();
    assert!(!monitor.on());
    let next = await_picture(&mut app, &monitor);
    assert!(next.b() > 240 && next.r() < 10);
    app.set_playing(false);
}

#[cfg(unix)]
#[test]
fn preview_slow_native_open_does_not_block_the_ui_and_reports_failure() {
    let fixture = Fixture::new();
    let fifo = fixture.dir.join("slow-input.mp4");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let writer_path = fifo.clone();
    let writer = thread::spawn(move || {
        let mut file = std::fs::File::create(writer_path).unwrap();
        thread::sleep(Duration::from_millis(600));
        file.write_all(b"deliberately invalid video after a slow native open")
            .unwrap();
    });
    let mut app = Editor::headless();
    add_asset(&mut app, &fixture.path);
    app.assets[0].info.path = fifo.to_string_lossy().into_owned();
    app.timeline.add_media_clip(
        "Slow input",
        ClipKind::Video,
        0,
        0.0,
        1.0,
        0.0,
        fifo.to_str().unwrap(),
    );
    app.set_playing(true);
    let started = Instant::now();
    app.sync_playback();
    let caller_time = started.elapsed();
    writer.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.status.starts_with("Video playback failed:") && Instant::now() < deadline {
        app.poll_frames();
        app.sync_playback();
        thread::sleep(Duration::from_millis(5));
    }
    eprintln!(
        "slow native open: caller={caller_time:?}; status={}",
        app.status
    );
    assert!(
        !app.playing,
        "a failed decoder must not consume an unseen clip"
    );
    let failed_generation = app.stream_generation;
    app.sync_playback();
    assert_eq!(app.stream_generation, failed_generation);
    assert!(
        app.still_inflight.is_none(),
        "a failure must not retry through the still worker"
    );
    app.set_playing(false);
    assert!(
        app.status.starts_with("Video playback failed:"),
        "{}",
        app.status
    );
    assert!(
        caller_time < Duration::from_millis(100),
        "UI waited {caller_time:?} for the native decoder"
    );
}

#[test]
fn jkl_shuttle_doubles_speed_runs_backward_and_stops_at_the_start() {
    let mut app = Editor::headless();
    app.timeline
        .add_clip("Card", ClipKind::Title, 0, 0.0, 10.0, 0.0);
    let mut time = 0.0;
    // Twenty 50 ms ticks: one second of wall clock.
    let mut run_second = |app: &mut Editor| {
        for _ in 0..20 {
            time += 0.05;
            app.advance_playback(time);
        }
    };
    app.playhead = 6.0;
    run_second(&mut app);
    app.perform(Action::ShuttleReverse);
    run_second(&mut app);
    assert!(
        (app.playhead - 5.0).abs() < 1e-6,
        "1x reverse: {}",
        app.playhead
    );
    app.perform(Action::ShuttleReverse);
    run_second(&mut app);
    assert!(
        (app.playhead - 3.0).abs() < 1e-6,
        "2x reverse: {}",
        app.playhead
    );
    assert!(!app.playing, "shuttling is not audio playback");

    // L after reverse is ordinary playback; L again is 2x forward.
    app.perform(Action::ShuttleForward);
    assert!(app.playing && app.shuttle == 0.0);
    app.perform(Action::ShuttleForward);
    assert_eq!((app.playing, app.shuttle), (false, 2.0));
    for _ in 0..4 {
        app.perform(Action::ShuttleForward);
    }
    assert_eq!(app.shuttle, SHUTTLE_MAX, "speed caps at 8x");
    app.perform(Action::ShuttleStop);
    assert_eq!((app.playing, app.shuttle), (false, 0.0));

    app.playhead = 0.5;
    app.perform(Action::ShuttleReverse);
    run_second(&mut app);
    assert_eq!(app.playhead, 0.0);
    assert_eq!(app.shuttle, 0.0, "reverse stops at the start");
}

use std::cell::RefCell;
use std::rc::Rc;

use super::*;

/// One script as one edit, the path `eddie run` (and later the live
/// server) takes: the request-time state, then every command or none.
mod script_edit_tests {
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
}

/// Generated sample media (video, music), made once per test run. Every
/// test shares the folder, so no test can remove it: the next run removes
/// the folders of runs whose process has gone.
fn sample_media() -> &'static [PathBuf] {
    const PREFIX: &str = "eddie-samples-";
    static SAMPLES: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    SAMPLES.get_or_init(|| {
        let temp = std::env::temp_dir();
        let running =
            |pid: &str| !Path::new("/proc/self").exists() || Path::new("/proc").join(pid).exists();
        for entry in std::fs::read_dir(&temp).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let pid = name.to_str().and_then(|name| name.strip_prefix(PREFIX));
            if pid.is_some_and(|pid| pid.parse::<u32>().is_ok() && !running(pid)) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let dir = temp.join(format!("{PREFIX}{}", std::process::id()));
        openreel_media::generate_samples(&dir).expect("generate samples")
    })
}

mod playback_tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

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

    #[test]
    fn preview_plays_at_the_loudness_export_would_apply() {
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
        app.match_export_loudness = true;
        add_asset(&mut app, &tone);
        let clip = app
            .timeline
            .add_media_clip("tone", ClipKind::Audio, 0, 0.0, 3.0, 0.0, &tone);
        let measured = |app: &mut Editor| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                app.loudness_checked = Instant::now() - Duration::from_secs(1);
                app.poll_loudness(false);
                if app.loudness_rx.is_none() {
                    if let Some(gain) = app.loudness_gain {
                        return 20.0 * gain.log10();
                    }
                }
                assert!(Instant::now() < deadline, "loudness never measured");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // FFmpeg's ebur128 reads this tone at -21.8 LUFS, so export's
        // gain to -16 LUFS is +5.8 dB.
        let first = measured(&mut app);
        assert!((first - 5.8).abs() < 0.3, "gain {first} dB");
        let report = app.audio_report();
        assert_eq!(report["status"], "measured");
        let lufs = report["integratedLufs"].as_f64().unwrap();
        assert!((lufs + 21.8).abs() < 0.3, "{report}");
        assert_eq!(report["clips"][0]["id"], clip);
        assert_eq!(report["limited"], false);
        assert_eq!(report["quietRanges"].as_array().unwrap().len(), 0);
        // Halving the clip's volume asks for 6 dB more.
        app.timeline.clip_mut(clip).unwrap().volume = 0.5;
        app.loudness_gain = None;
        let second = measured(&mut app);
        assert!(
            (second - first - 6.02).abs() < 0.1,
            "{first} then {second} dB"
        );
    }

    /// Without a window there is no preview to keep at the export's
    /// loudness, so the mix is measured when a request reads it.
    #[test]
    fn a_request_that_reads_the_mix_waits_for_its_measurement() {
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
        assert_eq!(app.audio_report()["status"], "silent");
        add_asset(&mut app, &tone);
        app.timeline
            .add_media_clip("tone", ClipKind::Audio, 0, 0.0, 3.0, 0.0, &tone);
        assert_eq!(app.audio_report()["status"], "measuring");

        let answer = |app: &mut Editor, method: &str, code: &str| {
            let params = serde_json::json!({ "code": code });
            let mut job = match commands::start(app, method, &params).unwrap() {
                Reply::Done(outcome) => return (outcome.value, false),
                Reply::Later(job) => job,
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                // Nothing else takes the measurement in: a frame's own poll
                // comes after its jobs.
                if let Some(result) = job.poll(app, &mut Vec::new()) {
                    return (result.unwrap().value, true);
                }
                assert!(Instant::now() < deadline, "the mix was never measured");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // A script that never looks at the mix does not pay for measuring it.
        let (run, waited) = answer(&mut app, "run", "return clips().length");
        assert_eq!((&run["output"], waited), (&"1".into(), false));
        // One that does gets the measurement, and its edit applies once.
        let script = "setVolume(clips()[0].id, 0.5); return audioReport().integratedLufs";
        let (run, waited) = answer(&mut app, "run", script);
        assert!(waited);
        // FFmpeg's ebur128 reads this tone at -21.8 LUFS.
        let lufs: f64 = run["output"].as_str().unwrap().parse().unwrap();
        assert!((lufs + 21.8).abs() < 0.3, "{run}");
        assert_eq!(app.timeline.clips[0].volume, 0.5);
        // The edit changed the mix, so `state` measures again.
        let (state, waited) = answer(&mut app, "state", "");
        assert!(waited);
        assert_eq!(state["audio"]["status"], "measured");
        let lufs = state["audio"]["integratedLufs"].as_f64().unwrap();
        assert!((lufs + 27.8).abs() < 0.3, "{state}");
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

    #[test]
    fn preview_source_remap_restarts_the_same_clip_and_invalidates_its_picture() {
        let fixture = Fixture::new();
        let (mut app, monitor) = watched_editor();
        add_asset(&mut app, &fixture.path);
        let clip = app.timeline.add_media_clip(
            "Excerpt",
            ClipKind::Video,
            0,
            0.0,
            0.5,
            0.0,
            &fixture.path,
        );
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

/// Free-form timeline: deleting a clip leaves its gap, magnet
/// on included. The magnet only snaps edges while dragging.
#[test]
fn delete_with_magnet_keeps_gap() {
    let mut app = Editor::headless();
    assert!(app.viewport.magnet);
    app.timeline
        .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 0.0);
    let b_id = app
        .timeline
        .add_clip("b.mp4", ClipKind::Video, 0, 10.0, 10.0, 0.0);
    let c_id = app
        .timeline
        .add_clip("c.mp4", ClipKind::Video, 0, 20.0, 10.0, 0.0);
    app.selected = Some(b_id);
    app.perform(Action::DeleteSelected);
    assert!(app.timeline.clip(b_id).is_none());
    let c = app.timeline.clip(c_id).unwrap();
    assert!(
        (c.start - 20.0).abs() < 1e-6,
        "magnet delete should leave the gap, got {}",
        c.start
    );
}

/// Split acts on the playhead, not on a click point: with no
/// selection it cuts the clip under the playhead.
#[test]
fn split_at_playhead_cuts_clip_under_playhead_without_selection() {
    let mut app = Editor::headless();
    let clip_id = app
        .timeline
        .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 2.0);
    app.selected = None;
    app.playhead = 4.0;
    app.perform(Action::SplitSelected);
    assert_eq!(app.timeline.clips.len(), 2);
    let left = app.timeline.clip(clip_id).unwrap();
    assert!((left.end() - 4.0).abs() < 0.02, "left end {}", left.end());
    let right_id = app.selected.expect("right half selected");
    let right = app.timeline.clip(right_id).unwrap();
    assert!((right.start - 4.0).abs() < 0.02);
    assert!((right.source_in - 6.0).abs() < 0.02);
}

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

#[test]
#[ignore = "needs OPENREEL_MONTAGE_DIR pointing at the local montage corpus"]
fn local_montage_corpus_curates_and_builds_recap_rehearsal() {
    let Ok(dir) = std::env::var("OPENREEL_MONTAGE_DIR") else {
        println!("SKIP local montage corpus: OPENREEL_MONTAGE_DIR is not set");
        return;
    };
    let mut pieces: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read montage dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ["mp4", "mov", "mkv", "webm"].contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    pieces.sort();
    assert!(!pieces.is_empty(), "no montage videos in {dir}");
    println!("montage corpus: {} file(s) in {dir}", pieces.len());

    let mut app = Editor::headless();
    let expected_assets = pieces.len();
    app.spawn_import(pieces);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
    let mut curation_started = false;
    loop {
        app.poll_imports();
        app.poll_sweeps();
        if app.import_rx.is_none() && app.assets.len() == expected_assets && !curation_started {
            app.curate_all_new();
            curation_started = true;
        }
        let done = curation_started
            && app.import_rx.is_none()
            && app.sweep_rx.is_none()
            && app.sweep_queue.is_empty()
            && app.assets.len() == expected_assets
            && app.assets.iter().all(|asset| {
                matches!(
                    asset.sweep.status,
                    sweep::SweepStatus::Ready | sweep::SweepStatus::Failed
                )
            });
        if done {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "montage import/curation timeout: {}",
            app.status
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    for asset in &app.assets {
        let beds = asset
            .sweep
            .candidates
            .iter()
            .filter(|candidate| candidate.music_bed)
            .count();
        let visual_candidates = asset
            .sweep
            .candidates
            .iter()
            .filter(|candidate| candidate.visual.is_some())
            .count();
        let covered: f64 = asset
            .sweep
            .scene_clips
            .iter()
            .map(|scene| scene.duration)
            .sum();
        println!(
            "curated {}: status={:?} picks={} scenes={} sceneCoverage={covered:.2}/{:.2}s musicBedPicks={} visualPicks={} note={}",
            asset.info.name,
            asset.sweep.status,
            asset.sweep.candidates.len(),
            asset.sweep.scene_clips.len(),
            asset.info.duration,
            beds,
            visual_candidates,
            asset.sweep.note,
        );
        assert!(
            !asset.sweep.scene_clips.is_empty(),
            "no scene clips for {}",
            asset.info.name
        );
        assert!(
            asset.sweep.scene_clips[0].source_in.abs() < 0.001,
            "scene breakdown starts at zero"
        );
        for pair in asset.sweep.scene_clips.windows(2) {
            assert!(
                (pair[0].end() - pair[1].source_in).abs() < 0.001,
                "scene breakdown is contiguous for {}",
                asset.info.name
            );
        }
        let last_scene = asset.sweep.scene_clips.last().expect("last scene");
        assert!(
            (last_scene.end() - asset.info.duration).abs() < 0.01,
            "scene breakdown ends at source duration for {}",
            asset.info.name
        );
        assert!(
            (covered - asset.info.duration).abs() < 0.01,
            "scene breakdown covers the whole source for {}",
            asset.info.name
        );
        let scene_ranges: Vec<String> = asset
            .sweep
            .scene_clips
            .iter()
            .map(|scene| format!("{:.2}-{:.2}", scene.source_in, scene.end()))
            .collect();
        println!(
            "scene ranges {}: {}",
            asset.info.name,
            scene_ranges.join(" ")
        );
        assert!(
            !asset.sweep.candidates.is_empty(),
            "no candidates for {}",
            asset.info.name
        );
        assert_eq!(
            visual_candidates,
            asset.sweep.candidates.len(),
            "every video candidate carries local visual evidence"
        );
        for candidate in &asset.sweep.candidates {
            let visual = candidate.visual_json();
            assert_eq!(visual["source"], "local-proxy");
            assert_eq!(visual["samStatus"], "unavailable");
            assert!(visual["facePresent"].is_null());
            assert!(visual["expression"].is_null());
        }
    }

    app.run_console(
        "var byAsset = {};\n\
         sweepCandidates().forEach(function (c) { if (c.hasVideo) { (byAsset[c.asset] = byAsset[c.asset] || []).push(c); } });\n\
         clips().forEach(function (c) { deleteClip(c.id); });\n\
         var cursor = 0;\n\
         addTitle('2025 RECAP', 0, 2, 'pop-caption');\n\
         Object.keys(byAsset).sort().forEach(function (asset) {\n\
           byAsset[asset].slice(0, 2).forEach(function (c) { var d = Math.min(c.duration, 3); addAssetRange(c.asset, cursor, c.sourceIn, d); cursor += d; });\n\
         });\n\
         console.log('Done — local recap rehearsal built.');"
            .to_owned(),
    );
    let errors: Vec<&String> = app
        .console_output
        .iter()
        .filter(|line| line.starts_with("error"))
        .collect();
    assert!(errors.is_empty(), "console errors: {errors:?}");
    let videos = app
        .timeline
        .clips
        .iter()
        .filter(|clip| clip.kind == ClipKind::Video)
        .count();
    assert!(videos >= expected_assets, "grouped picks landed: {videos}");
    assert!(app.timeline.clips.iter().any(|clip| {
        clip.kind == ClipKind::Title && clip.title_style == openreel_engine::TitleStyle::PopCaption
    }));
    let expected_end = app.timeline.end();

    let output = std::env::var("OPENREEL_RECAP_OUTPUT")
        .unwrap_or_else(|_| "/tmp/recap_2025_local_rehearsal.mp4".to_owned());
    let _ = std::fs::remove_file(&output);
    app.run_console(format!("exportTo('{output}')"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        app.poll_export();
        if app.export_rx.is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "recap export timeout: {}",
            app.status
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(app.status.starts_with("Exported "), "{}", app.status);
    let info = openreel_media::probe(&output).expect("probe local recap");
    assert!(
        info.has_video && info.has_audio,
        "recap must contain picture and sound"
    );
    println!(
        "local recap: {:.2}s clips={} status={} output={}",
        info.duration,
        app.timeline.clips.len(),
        app.status,
        output,
    );
    assert!((info.duration - expected_end).abs() < 0.6);
}

#[test]
fn video_of_the_other_orientation_is_never_added_to_the_project() {
    let mut app = Editor::headless();
    // Displayed sizes: a rotated phone clip probes as portrait.
    for (name, width, height) in [("wide.mp4", 1920, 1080), ("phone.mov", 1080, 1920)] {
        let path = format!("/media/{name}");
        let scene = sweep::SweepCandidate {
            id: format!("{path}#scene-0"),
            asset_path: path.clone(),
            asset_name: name.to_owned(),
            title: name.to_owned(),
            source_in: 1.0,
            duration: 2.0,
            score: 80.0,
            reason: "Detected shot.".to_owned(),
            stability: 0.9,
            audio_energy: 0.4,
            audio_peak: 0.7,
            category: "broll".to_owned(),
            tags: Vec::new(),
            motion_score: 0.3,
            audio_score: 0.5,
            music_bed: false,
            visual: None,
            has_video: true,
            transcript: Vec::new(),
            review: sweep::ClipReview::default(),
            dismissed: false,
        };
        app.assets.push(BinAsset {
            info: MediaInfo {
                path,
                name: name.to_owned(),
                duration: 10.0,
                width,
                height,
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
                candidates: vec![scene.clone()],
                scene_clips: vec![scene],
                ..Default::default()
            },
        });
    }

    // Bulk adds skip the clashing source instead of leaving its slot.
    app.add_all_scene_clips();
    let sources: Vec<_> = app
        .timeline
        .clips
        .iter()
        .map(|c| c.source.clone())
        .collect();
    assert_eq!(sources, vec![Some("/media/wide.mp4".to_owned())]);
    assert_eq!(app.timeline.end(), 2.0);
    assert!(app.status.contains("skipped 1"), "{}", app.status);

    // Every single-add path, scripts included, shares the refusal.
    let refused = app.try_add_asset_range_to_timeline("/media/phone.mov", "", None, 5.0, 0.0, 1.0);
    assert_eq!(
        refused,
        Err("phone.mov is portrait; the project is landscape.".to_owned())
    );
    assert_eq!(app.timeline.clips.len(), 1);

    // A portrait project accepts it, and the canvas stays portrait.
    app.set_format(Orientation::Portrait);
    assert!(
        app.status.contains("1 clip doesn't match"),
        "{}",
        app.status
    );
    app.try_add_asset_range_to_timeline("/media/phone.mov", "", None, 5.0, 0.0, 1.0)
        .expect("portrait video fits a portrait project");
    app.perform(Action::ClearTimeline);
    assert_eq!(app.timeline.orientation, Orientation::Portrait);
}

#[test]
fn adding_all_scene_clips_reconstructs_the_source_in_order() {
    let mut app = Editor::headless();
    let scene = |index: usize, source_in: f64, duration: f64, score: f32| sweep::SweepCandidate {
        id: format!("/media/montage.mp4#scene-{index}"),
        asset_path: "/media/montage.mp4".to_owned(),
        asset_name: "montage.mp4".to_owned(),
        title: format!("montage.mp4 · scene {index}"),
        source_in,
        duration,
        score,
        reason: "Detected shot.".to_owned(),
        stability: 0.9,
        audio_energy: 0.4,
        audio_peak: 0.7,
        category: "broll".to_owned(),
        tags: Vec::new(),
        motion_score: 0.3,
        audio_score: 0.5,
        music_bed: false,
        visual: None,
        has_video: true,
        transcript: Vec::new(),
        review: sweep::ClipReview::default(),
        dismissed: false,
    };
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/montage.mp4".to_owned(),
            name: "montage.mp4".to_owned(),
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
            candidates: vec![scene(99, 8.0, 2.0, 99.0)],
            scene_clips: vec![
                scene(2, 8.0, 7.0, 10.0),
                scene(0, 0.0, 3.0, 90.0),
                scene(3, 15.0, 5.0, 50.0),
                scene(1, 3.0, 5.0, 70.0),
            ],
            ..Default::default()
        },
    });

    app.add_all_scene_clips();
    let mut clips: Vec<_> = app.timeline.clips.iter().collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    assert_eq!(clips.len(), 4, "scene action ignores ranked picks");
    assert_eq!(
        clips.iter().map(|clip| clip.source_in).collect::<Vec<_>>(),
        vec![0.0, 3.0, 8.0, 15.0]
    );
    for pair in clips.windows(2) {
        assert_eq!(pair[0].end(), pair[1].start);
    }
    assert_eq!(app.timeline.end(), 20.0);

    for boundaries in [
        vec![
            0.0,
            1.0 / 3.0,
            49.0 / 60.0,
            1.235,
            1.585,
            2.135,
            3.2866666666666666,
        ],
        vec![0.0, 1.0 / 120.0, 1.0 / 60.0, 0.157, 0.158, 0.5],
    ] {
        app.assets[0].info.duration = *boundaries.last().unwrap();
        app.assets[0].sweep.scene_clips = boundaries
            .windows(2)
            .enumerate()
            .map(|(index, pair)| scene(index, pair[0], pair[1] - pair[0], 70.0))
            .rev()
            .collect();
        let count = boundaries.len() - 1;
        for route in ["all scenes", "individual", "console"] {
            app.timeline = Timeline::empty_project();
            for batch in 0..2 {
                let start = app.timeline.end();
                match route {
                    "all scenes" => app.add_all_scene_clips(),
                    "individual" => {
                        for index in 0..count {
                            app.add_sweep_candidate(&format!("/media/montage.mp4#scene-{index}"));
                        }
                    }
                    "console" => app.run_console(
                        "(() => { let at = clips().reduce((end, c) => Math.max(end, c.start + c.duration), 0); sceneClips().sort((a, b) => a.sourceIn - b.sourceIn).forEach(c => { addAssetRange(c.asset, at, c.sourceIn, c.duration); at += c.duration; }); })()".to_owned(),
                    ),
                    _ => unreachable!(),
                }
                assert_eq!(app.timeline.clips.len(), (batch + 1) * count, "{route}");
                assert_eq!(app.timeline.track_labels.len(), 1, "{route}: one free lane");
                let clips = &app.timeline.clips[batch * count..];
                for (clip, pair) in clips.iter().zip(boundaries.windows(2)) {
                    assert_eq!(clip.track, 0, "{route}");
                    assert!(
                        (clip.start - start - pair[0]).abs() < 1e-12,
                        "{route}: {clip:?}"
                    );
                    assert_eq!(clip.source_in, pair[0], "{route}");
                    assert_eq!(clip.duration, pair[1] - pair[0], "{route}");
                }
                for pair in app.timeline.clips.windows(2) {
                    assert_eq!(
                        pair[0].end(),
                        pair[1].start,
                        "{route}: contiguous placement"
                    );
                }
                assert!((app.timeline.end() - start - boundaries.last().unwrap()).abs() < 1e-12);
            }
        }
    }

    let placed = app.timeline.clips.clone();
    let gap = app
        .try_add_asset_range_to_timeline(
            "/media/montage.mp4",
            "after a gap",
            None,
            3.015,
            0.0,
            0.137,
        )
        .unwrap();
    let overlap = app
        .try_add_asset_range_to_timeline("/media/montage.mp4", "overlap", None, 3.04, 0.1, 0.01)
        .unwrap();
    let explicit = app
        .try_add_asset_range_to_timeline(
            "/media/montage.mp4",
            "explicit lane",
            Some(0),
            3.045,
            0.2,
            0.01,
        )
        .unwrap();
    let clamped = app
        .try_add_asset_range_to_timeline("/media/montage.mp4", "source end", None, 4.0, 0.49, 1.0)
        .unwrap();
    assert_eq!(
        &app.timeline.clips[..placed.len()],
        placed.as_slice(),
        "adds do not ripple existing clips"
    );
    assert_eq!(app.timeline.clip(gap).unwrap().start, 3.015);
    assert_eq!(app.timeline.clip(gap).unwrap().track, 0);
    assert_eq!(app.timeline.clip(overlap).unwrap().track, 1);
    assert_eq!(app.timeline.clip(explicit).unwrap().track, 0);
    assert!((app.timeline.clip(clamped).unwrap().duration - 0.01).abs() < 1e-12);
}

#[test]
fn adding_a_music_bed_range_starts_muted() {
    let mut app = Editor::headless();
    let candidate = |music_bed: bool| sweep::SweepCandidate {
        id: "/media/montage.mp4#0".to_owned(),
        asset_path: "/media/montage.mp4".to_owned(),
        asset_name: "montage.mp4".to_owned(),
        title: "montage.mp4 · 0:00".to_owned(),
        source_in: 0.0,
        duration: 20.0,
        score: 80.0,
        reason: "test".to_owned(),
        stability: 0.9,
        audio_energy: 0.6,
        audio_peak: 0.9,
        category: "broll".to_owned(),
        tags: Vec::new(),
        motion_score: 0.3,
        audio_score: 0.8,
        music_bed,
        visual: None,
        has_video: true,
        transcript: Vec::new(),
        review: sweep::ClipReview::default(),
        dismissed: false,
    };
    let asset = |music_bed: bool| BinAsset {
        info: MediaInfo {
            path: "/media/montage.mp4".to_owned(),
            name: "montage.mp4".to_owned(),
            duration: 60.0,
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
            candidates: vec![candidate(music_bed)],
            ..Default::default()
        },
    };
    app.assets.push(asset(true));
    app.add_asset_range_to_timeline("/media/montage.mp4", "bed", None, 0.0, 2.0, 6.0);
    let muted = app.timeline.clips.last().expect("clip added");
    assert_eq!(muted.volume, 0.0, "music-bed range starts muted");

    app.mute_music_beds = false;
    app.add_asset_range_to_timeline("/media/montage.mp4", "bed again", None, 20.0, 2.0, 6.0);
    let audible = app.timeline.clips.last().expect("clip added");
    assert_eq!(
        audible.volume, 1.0,
        "the same music bed stays audible when the user opts out of muting"
    );

    app.mute_music_beds = true;
    app.assets[0].sweep.scene_clips = vec![candidate(true)];
    let mut speech = candidate(false);
    speech.source_in = 2.0;
    speech.duration = 6.0;
    speech.review.status = sweep::ReviewStatus::Reviewed;
    app.assets[0].sweep.candidates = vec![speech];
    app.add_asset_range_to_timeline(
        "/media/montage.mp4",
        "reviewed speech",
        None,
        30.0,
        2.0,
        6.0,
    );
    assert_eq!(
        app.timeline.clips.last().unwrap().volume,
        1.0,
        "range-specific speech review overrides a whole-scene music hint"
    );
    app.add_asset_range_to_timeline("/media/montage.mp4", "unreviewed bed", None, 40.0, 8.0, 6.0);
    assert_eq!(
        app.timeline.clips.last().unwrap().volume,
        0.0,
        "unreviewed portions retain the scene evidence"
    );

    // A curated audio-only track is the new soundtrack, not baked-in score.
    let mut song = asset(true);
    song.info.path = "/media/song.mp3".to_owned();
    song.info.name = "song.mp3".to_owned();
    song.info.has_video = false;
    song.sweep.candidates[0].has_video = false;
    song.sweep.candidates[0].asset_path = song.info.path.clone();
    app.assets.push(song);
    app.add_asset_range_to_timeline("/media/song.mp3", "song", None, 0.0, 0.0, 6.0);
    assert_eq!(
        app.timeline.clips.last().unwrap().volume,
        1.0,
        "an added music track is never muted as a music bed"
    );
}

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

#[test]
fn curator_add_shown_uses_the_selected_source_and_filter_but_all_scenes_stays_complete() {
    let mut app = Editor::headless();
    for path in ["/media/a.mp4", "/media/b.mp4"] {
        let candidates: Vec<_> = ["action", "reaction", "reaction", "talking"]
            .into_iter()
            .enumerate()
            .map(|(index, category)| sweep::SweepCandidate {
                id: format!("{path}#clip-{index}"),
                asset_path: path.to_owned(),
                asset_name: path.to_owned(),
                title: category.to_owned(),
                source_in: index as f64 * 2.0,
                duration: 2.0,
                score: 80.0 + index as f32,
                reason: "Fixture range".to_owned(),
                stability: 0.9,
                audio_energy: 0.0,
                audio_peak: 0.0,
                category: category.to_owned(),
                tags: if index == 0 {
                    vec!["prime".to_owned()]
                } else {
                    Vec::new()
                },
                motion_score: 0.3,
                audio_score: 0.0,
                music_bed: false,
                visual: None,
                has_video: true,
                transcript: Vec::new(),
                review: sweep::ClipReview {
                    status: if index == 2 {
                        sweep::ReviewStatus::NeedsReview
                    } else {
                        sweep::ReviewStatus::Reviewed
                    },
                    ..Default::default()
                },
                dismissed: index == 1,
            })
            .collect();
        let mut scenes = candidates.clone();
        scenes[1].review.status = sweep::ReviewStatus::Rejected;
        app.assets.push(BinAsset {
            info: MediaInfo {
                path: path.to_owned(),
                name: path.to_owned(),
                duration: 8.0,
                width: 64,
                height: 36,
                fps: 30.0,
                has_video: true,
                has_audio: false,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData {
                candidates,
                scene_clips: scenes,
                ..Default::default()
            },
        });
    }
    app.selected_asset = Some("/media/b.mp4".to_owned());
    for (filter, expected) in [("reaction", 4.0), ("needs-review", 4.0), ("prime", 0.0)] {
        app.timeline = Timeline::empty_project();
        app.curator.filters = gallery::GalleryFilters::default();
        match filter {
            "prime" => app.curator.filters.quality = gallery::GalleryQuality::Prime,
            "needs-review" => app.curator.filters.quality = gallery::GalleryQuality::NeedsReview,
            category => app.curator.filters.category = Some(category.to_owned()),
        }
        app.add_all_sweep_candidates();
        assert_eq!(app.timeline.clips.len(), 1, "only the shown {filter} range");
        let clip = &app.timeline.clips[0];
        assert_eq!(clip.source.as_deref(), Some("/media/b.mp4"));
        assert_eq!((clip.source_in, clip.duration), (expected, 2.0));
    }
    app.timeline = Timeline::empty_project();
    app.add_all_scene_clips();
    assert_eq!(
        app.timeline.clips.len(),
        4,
        "category and dismissals never hide source scenes"
    );
    for (index, clip) in app.timeline.clips.iter().enumerate() {
        assert_eq!(clip.source.as_deref(), Some("/media/b.mp4"));
        assert_eq!(
            (clip.start, clip.source_in, clip.duration),
            (index as f64 * 2.0, index as f64 * 2.0, 2.0)
        );
    }
    let snapshot: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    assert_eq!(
        snapshot["sceneClips"].as_array().unwrap().len(),
        8,
        "REPL inventory is not the UI filter"
    );
    assert_eq!(
        snapshot["sweepCandidates"].as_array().unwrap().len(),
        6,
        "only explicit dismissals hide suggestions"
    );
}

#[test]
fn gallery_add_shown_combines_filters_and_preserves_display_order() {
    use gallery::{GalleryDuration, GalleryFilters, GalleryQuality, GallerySort};

    let mut app = Editor::headless();
    for (path, name) in [
        ("/media/expo.mp4", "Expo.mp4"),
        ("/media/interview.mov", "Interview.mov"),
    ] {
        let candidates: Vec<_> = [
            (0.25, 1.5, 82.0, "action", "Rocket launch ready"),
            (10.0, 2.0, 96.0, "action", "Rocket launch now"),
            (20.0, 7.5, 87.0, "action", "Launch later"),
            (40.0, 30.0, 88.0, "talking", "Rocket payload"),
            (80.0, 8.0, 77.0, "action", "Slow launch"),
            (100.0, 29.5, 80.0, "action", "Rocket flame"),
        ]
        .into_iter()
        .enumerate()
        .map(
            |(index, (source_in, duration, score, category, text))| sweep::SweepCandidate {
                id: format!("{path}#pick-{index}"),
                asset_path: path.to_owned(),
                asset_name: name.to_owned(),
                title: format!("Take {index}"),
                source_in,
                duration,
                score,
                reason: if index == 2 {
                    "Crowd portrait"
                } else {
                    "Useful range"
                }
                .to_owned(),
                stability: 0.9,
                audio_energy: 0.1,
                audio_peak: 0.2,
                category: category.to_owned(),
                tags: vec![if matches!(index, 0 | 1 | 3 | 5) {
                    "prime"
                } else {
                    "steady"
                }
                .to_owned()],
                motion_score: 0.3,
                audio_score: 0.4,
                music_bed: false,
                visual: None,
                has_video: true,
                transcript: vec![sweep::TranscriptSegment {
                    start: source_in,
                    end: source_in + duration,
                    speaker: None,
                    text: text.to_owned(),
                    words: Vec::new(),
                    session: None,
                }],
                review: sweep::ClipReview {
                    status: match index {
                        2 => sweep::ReviewStatus::NeedsReview,
                        4 => sweep::ReviewStatus::Local,
                        _ => sweep::ReviewStatus::Reviewed,
                    },
                    ..Default::default()
                },
                dismissed: index == 5,
            },
        )
        .collect();
        let mut scene = candidates[0].clone();
        scene.id = format!("{path}#scene-0");
        scene.source_in = 0.0;
        scene.duration = 200.0;
        scene.review.status = sweep::ReviewStatus::Rejected;
        app.assets.push(BinAsset {
            info: MediaInfo {
                path: path.to_owned(),
                name: name.to_owned(),
                duration: 200.0,
                width: 64,
                height: 36,
                fps: 30.0,
                has_video: true,
                has_audio: false,
            },
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData {
                status: sweep::SweepStatus::Ready,
                candidates,
                scene_clips: vec![scene],
                ..Default::default()
            },
        });
    }
    let before: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    // Review choices are offered only while the bin holds a curation the
    // removed reviewer saved.
    assert_eq!(
        app.gallery_quality_choices(),
        [
            GalleryQuality::All,
            GalleryQuality::Prime,
            GalleryQuality::Reviewed,
            GalleryQuality::NeedsReview,
            GalleryQuality::Local,
        ]
    );
    fn candidates(app: &mut Editor) -> impl Iterator<Item = &mut sweep::SweepCandidate> {
        app.assets
            .iter_mut()
            .flat_map(|asset| &mut asset.sweep.candidates)
    }
    let saved: Vec<_> = candidates(&mut app)
        .map(|clip| std::mem::take(&mut clip.review.status))
        .collect();
    assert_eq!(
        app.gallery_quality_choices(),
        [GalleryQuality::All, GalleryQuality::Prime]
    );
    for (clip, status) in candidates(&mut app).zip(saved) {
        clip.review.status = status;
    }
    app.selected_asset = Some("/media/interview.mov".to_owned());
    let add_shown = |app: &mut Editor| -> Vec<(f64, f64)> {
        app.timeline = Timeline::empty_project();
        app.add_all_sweep_candidates();
        assert!(app
            .timeline
            .clips
            .iter()
            .all(|clip| clip.source.as_deref() == Some("/media/interview.mov")));
        app.timeline
            .clips
            .iter()
            .map(|clip| (clip.source_in, clip.duration))
            .collect()
    };
    app.curator.filters = GalleryFilters {
        search: "  ROCKET  launch ".to_owned(),
        category: Some("action".to_owned()),
        quality: GalleryQuality::Prime,
        duration: GalleryDuration::TwoToEight,
        ..Default::default()
    };
    assert_eq!(
        add_shown(&mut app),
        [(10.0, 2.0)],
        "every filter must match"
    );
    app.curator.filters.duration = GalleryDuration::All;
    assert_eq!(add_shown(&mut app), [(10.0, 2.0), (0.25, 1.5)]);

    for (search, expected) in [
        ("interview rocket payload", vec![(40.0, 30.0)]),
        ("CROWD steady", vec![(20.0, 7.5)]),
        ("flame", Vec::new()),
        ("not-in-any-clip", Vec::new()),
    ] {
        app.curator.filters = GalleryFilters {
            search: search.to_owned(),
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected, "search {search}");
    }
    for (quality, expected) in [
        (GalleryQuality::NeedsReview, vec![(20.0, 7.5)]),
        (GalleryQuality::Local, vec![(80.0, 8.0)]),
        (
            GalleryQuality::Reviewed,
            vec![(10.0, 2.0), (40.0, 30.0), (0.25, 1.5)],
        ),
    ] {
        app.curator.filters = GalleryFilters {
            quality,
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected);
    }
    for (duration, expected) in [
        (GalleryDuration::UnderTwo, vec![(0.25, 1.5)]),
        (GalleryDuration::TwoToEight, vec![(10.0, 2.0), (20.0, 7.5)]),
        (GalleryDuration::EightToThirty, vec![(80.0, 8.0)]),
        (GalleryDuration::ThirtyPlus, vec![(40.0, 30.0)]),
    ] {
        app.curator.filters = GalleryFilters {
            duration,
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected, "half-open duration bands");
    }
    for (sort, expected) in [
        (
            GallerySort::Score,
            // The Needs review range sorts after every usable one.
            vec![
                (10.0, 2.0),
                (40.0, 30.0),
                (0.25, 1.5),
                (80.0, 8.0),
                (20.0, 7.5),
            ],
        ),
        (
            GallerySort::Source,
            vec![
                (0.25, 1.5),
                (10.0, 2.0),
                (20.0, 7.5),
                (40.0, 30.0),
                (80.0, 8.0),
            ],
        ),
        (
            GallerySort::Shortest,
            vec![
                (0.25, 1.5),
                (10.0, 2.0),
                (20.0, 7.5),
                (80.0, 8.0),
                (40.0, 30.0),
            ],
        ),
        (
            GallerySort::Longest,
            vec![
                (40.0, 30.0),
                (80.0, 8.0),
                (20.0, 7.5),
                (10.0, 2.0),
                (0.25, 1.5),
            ],
        ),
    ] {
        app.curator.filters = GalleryFilters {
            sort,
            ..Default::default()
        };
        assert_eq!(add_shown(&mut app), expected);
        let mut cursor = 0.0;
        for clip in &app.timeline.clips {
            assert_eq!(clip.start, cursor, "display order becomes timeline order");
            cursor += clip.duration;
        }
    }
    app.selected_asset = None;
    app.curator.filters = GalleryFilters {
        search: "rocket launch".to_owned(),
        quality: GalleryQuality::Prime,
        duration: GalleryDuration::TwoToEight,
        ..Default::default()
    };
    app.timeline = Timeline::empty_project();
    app.add_all_sweep_candidates();
    assert_eq!(app.timeline.clips.len(), 2);
    assert_eq!(
        app.timeline.clips[0].source.as_deref(),
        Some("/media/expo.mp4")
    );
    assert_eq!(
        app.timeline.clips[1].source.as_deref(),
        Some("/media/interview.mov")
    );

    app.curator.filters.search = "no matches".to_owned();
    app.timeline = Timeline::empty_project();
    app.add_all_scene_clips();
    assert_eq!(
        app.timeline.clips.len(),
        2,
        "All scenes bypasses every Clip filter"
    );
    assert_eq!(
        (app.timeline.clips[0].start, app.timeline.clips[0].duration),
        (0.0, 200.0)
    );
    assert_eq!(
        (app.timeline.clips[1].start, app.timeline.clips[1].duration),
        (200.0, 200.0)
    );
    let after: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
    assert_eq!(before["sweepCandidates"], after["sweepCandidates"]);
    assert_eq!(before["sceneClips"], after["sceneClips"]);
}

#[test]
fn curation_waits_for_explicit_curate_action() {
    let mut app = Editor::headless();
    app.assets.push(BinAsset {
        info: MediaInfo {
            path: "/media/not-curated-yet.mp4".to_owned(),
            name: "not-curated-yet.mp4".to_owned(),
            duration: 10.0,
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
        sweep: sweep::SweepData::default(),
    });

    assert_eq!(
        app.assets[0].sweep.status,
        sweep::SweepStatus::NotStarted,
        "a ready import stays untouched until Curate is pressed"
    );
    assert!(app.sweep_rx.is_none());
    assert!(app.sweep_queue.is_empty());
    assert!(!app.curator.open, "import does not open the curator");

    app.playing = true;
    app.curate_asset("/media/not-curated-yet.mp4".to_owned());
    assert_eq!(app.assets[0].sweep.status, sweep::SweepStatus::Running);
    assert!(app.sweep_rx.is_some(), "explicit Curate starts one worker");
    assert!(!app.playing, "gallery pauses playback");
    assert!(app.curator.open, "curation opens the results gallery");
    assert!(app.timeline.clips.is_empty(), "browsing never adds clips");

    let (tx, rx) = channel();
    app.sweep_rx = Some(rx);
    app.curator.open = false;
    tx.send(SweepMsg::Done {
        path: "/media/not-curated-yet.mp4".to_owned(),
        data: sweep::SweepData {
            status: sweep::SweepStatus::Ready,
            ..Default::default()
        },
    })
    .unwrap();
    app.poll_sweeps();
    assert_eq!(app.assets[0].sweep.status, sweep::SweepStatus::Ready);
    assert!(!app.curator.open, "completion respects a return to Program");
    assert!(app.timeline.clips.is_empty());
    app.open_curator_gallery();
    app.set_playing(true);
    assert!(!app.curator.open, "playback returns to Program");
}

#[test]
fn saved_curation_returns_with_the_same_file_only() {
    let dir = std::env::temp_dir().join(format!("eddie-saved-curation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("two-shots.mp4");
    let render = |second: &str| {
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=64x36:r=10:d=2",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("color={second}:s=64x36:r=10:d=2"),
            ])
            .args([
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
            ])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
    };
    render("blue");
    let media = path.to_string_lossy().into_owned();
    let session = |curate: bool| {
        let mut app = Editor::headless();
        app.curation_dir = Some(dir.join("store"));
        app.spawn_import(vec![path.clone()]);
        let deadline = Instant::now() + std::time::Duration::from_secs(120);
        let mut curated = !curate;
        loop {
            app.poll_imports();
            app.poll_sweeps();
            let imported = app.import_rx.is_none() && app.assets[0].state == AssetState::Ready;
            if imported && !curated {
                app.curate_asset(media.clone());
                curated = true;
            }
            if imported && curated && app.sweep_rx.is_none() {
                break;
            }
            assert!(Instant::now() < deadline, "import or curation timed out");
            thread::sleep(std::time::Duration::from_millis(20));
        }
        let state: serde_json::Value = serde_json::from_str(&app.script_state_json()).unwrap();
        (
            app.assets[0].sweep.status,
            state["sweepCandidates"].clone(),
            state["sceneClips"].clone(),
        )
    };

    let (status, candidates, scenes) = session(true);
    assert_eq!(status, sweep::SweepStatus::Ready);
    assert!(scenes.as_array().unwrap().len() >= 2, "two shots: {scenes}");

    let (status, restored_candidates, restored_scenes) = session(false);
    assert_eq!(
        status,
        sweep::SweepStatus::Ready,
        "a reopened file keeps its curation"
    );
    assert_eq!(restored_candidates, candidates);
    assert_eq!(restored_scenes, scenes);

    std::thread::sleep(std::time::Duration::from_millis(20));
    render("green");
    let (status, candidates, _) = session(false);
    assert_eq!(
        status,
        sweep::SweepStatus::NotStarted,
        "a replaced file is not matched"
    );
    assert_eq!(candidates, serde_json::json!([]));
    let _ = std::fs::remove_dir_all(&dir);
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
fn curate_all_new_overlaps_files_and_finishes_every_one() {
    let dir = std::env::temp_dir().join(format!("eddie-parallel-curation-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut app = Editor::headless();
    let colors = ["red", "green", "blue", "yellow"];
    for color in colors {
        let path = dir.join(format!("{color}.mp4"));
        let output = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("color={color}:s=64x36:r=10:d=2"))
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-threads", "1"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        app.assets.push(BinAsset {
            info: openreel_media::probe(path.to_str().unwrap()).unwrap(),
            size_bytes: 0,
            state: AssetState::Ready,
            peaks: Vec::new(),
            beats: None,
            thumbs: Vec::new(),
            sweep: sweep::SweepData::default(),
        });
    }

    app.curate_all_new();
    let active = app
        .assets
        .iter()
        .filter(|asset| {
            asset.sweep.status == sweep::SweepStatus::Running
                && !app.sweep_queue.contains(&asset.info.path)
        })
        .count();
    let expected = std::thread::available_parallelism().map_or(1, |n| (n.get() / 4).clamp(1, 3));
    assert_eq!(active, expected, "several files curate at once");
    assert_eq!(app.sweep_queue.len(), colors.len() - expected);

    let deadline = Instant::now() + std::time::Duration::from_secs(120);
    while app.sweep_rx.is_some() || !app.sweep_queue.is_empty() {
        app.poll_sweeps();
        assert!(Instant::now() < deadline, "curation timed out");
        thread::sleep(std::time::Duration::from_millis(20));
    }
    for asset in &app.assets {
        assert_eq!(
            asset.sweep.status,
            sweep::SweepStatus::Ready,
            "{}",
            asset.info.name
        );
        assert!(!asset.sweep.scene_clips.is_empty(), "{}", asset.info.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn delete_without_magnet_keeps_gap() {
    let mut app = Editor::headless();
    app.viewport.magnet = false;
    app.timeline
        .add_clip("a.mp4", ClipKind::Video, 0, 0.0, 10.0, 0.0);
    let b_id = app
        .timeline
        .add_clip("b.mp4", ClipKind::Video, 0, 10.0, 10.0, 0.0);
    let c_id = app
        .timeline
        .add_clip("c.mp4", ClipKind::Video, 0, 20.0, 10.0, 0.0);
    app.selected = Some(b_id);
    app.perform(Action::DeleteSelected);
    let c = app.timeline.clip(c_id).unwrap();
    assert!(
        (c.start - 20.0).abs() < 1e-6,
        "magnet off should leave the gap, got {}",
        c.start
    );
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

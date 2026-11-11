//! The preview plays at the loudness export will apply.

use super::*;

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

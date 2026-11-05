//! In-process audio: timeline placement, source offsets, volume, mixing and
//! the picture-gated clock, against real libavcodec decoding.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use openreel_media::{AudioEngine, AudioSegment, AUDIO_RATE};

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

/// Silence, then a 1 kHz tone from exactly `onset` seconds to the end.
fn tone_file(name: &str, onset: f64, codec: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eddie-audio-{}-{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!(
            "aevalsrc='if(gte(t,{onset}),0.5*sin(2*PI*1000*t),0)|if(gte(t,{onset}),0.5*sin(2*PI*1000*t),0)':s=44100:d=4"
        ))
        .args(codec)
        .arg(&path)
        .status()
        .expect("ffmpeg");
    assert!(status.success());
    path
}

fn segment(path: &Path, start: f64, duration: f64, source_in: f64, volume: f32) -> AudioSegment {
    AudioSegment {
        path: path.to_string_lossy().into_owned(),
        start,
        duration,
        source_in,
        volume,
        fade_in: 0.0,
        fade_out: 0.0,
        duck: Vec::new(),
    }
}

/// Render `seconds` of timeline from the engine's current clock.
fn render(engine: &mut AudioEngine, seconds: f64) -> Vec<f32> {
    let mut out = Vec::new();
    let frames = (seconds * f64::from(AUDIO_RATE)).round() as usize;
    while out.len() < frames * 2 {
        let want = (frames - out.len() / 2).min(1024);
        let block = engine.render(want);
        assert!(!block.is_empty(), "clock stopped at {}", engine.clock());
        out.extend_from_slice(block);
    }
    out
}

/// Peak absolute left-channel level over timeline seconds `[from, to)`.
fn peak(mix: &[f32], from: f64, to: f64) -> f32 {
    let rate = f64::from(AUDIO_RATE);
    let (a, b) = ((from * rate) as usize, (to * rate) as usize);
    mix[a * 2..b * 2]
        .chunks_exact(2)
        .map(|frame| frame[0].abs())
        .fold(0.0, f32::max)
}

/// Timeline second of the first frame louder than 0.05.
fn onset(mix: &[f32]) -> f64 {
    let index = mix
        .chunks_exact(2)
        .position(|frame| frame[0].abs() > 0.05)
        .expect("tone never starts");
    index as f64 / f64::from(AUDIO_RATE)
}

fn playing(segments: &[(u64, AudioSegment)]) -> AudioEngine {
    let mut engine = AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(f64::MAX / 1e6);
    engine.set_running(true);
    engine.set_voices(segments);
    engine
}

#[test]
fn source_offset_and_timeline_start_place_the_tone_exactly() {
    // Tone at source 1.5 s; clip at timeline 0.5 s from source 1.0 s, so it
    // must be heard from timeline 1.0 s until the clip ends at 1.75 s.
    for (name, codec) in [
        ("tone.wav", &["-c:a", "pcm_s16le"][..]),
        ("tone.m4a", &["-c:a", "aac", "-b:a", "192k"][..]),
        ("tone.mp3", &["-c:a", "libmp3lame", "-b:a", "192k"][..]),
    ] {
        let path = tone_file(name, 1.5, codec);
        let mut engine = playing(&[(1, segment(&path, 0.5, 1.25, 1.0, 1.0))]);
        let mix = render(&mut engine, 2.5);
        let heard = onset(&mix);
        assert!((heard - 1.0).abs() < 0.003, "{name}: onset {heard}");
        assert!(peak(&mix, 1.1, 1.7) > 0.45, "{name}: tone missing");
        assert_eq!(peak(&mix, 1.75, 2.5), 0.0, "{name}: plays past clip end");
        assert!((engine.clock() - 2.5).abs() < 1e-9);
    }
}

#[test]
fn starting_mid_clip_seeks_into_the_source() {
    // A seek to timeline 2.0 inside a clip from source 0 must start at
    // source 2.0, not replay the clip's beginning.
    let path = tone_file("tone.m4a", 2.5, &["-c:a", "aac"]);
    let mut engine = AudioEngine::manual();
    engine.seek(2.0);
    engine.set_limit(10.0);
    engine.set_running(true);
    engine.set_voices(&[(7, segment(&path, 0.0, 4.0, 0.0, 1.0))]);
    let mix = render(&mut engine, 1.0);
    let heard = 2.0 + onset(&mix);
    assert!((heard - 2.5).abs() < 0.003, "onset {heard}");
}

#[test]
fn volume_scales_and_voices_mix() {
    let path = tone_file("tone.wav", 0.0, &["-c:a", "pcm_s16le"]);
    let mut half = playing(&[(1, segment(&path, 0.0, 1.0, 0.0, 0.5))]);
    let quiet = peak(&render(&mut half, 1.0), 0.1, 0.9);
    assert!((quiet - 0.25).abs() < 0.01, "{quiet}");

    let mut both = playing(&[
        (1, segment(&path, 0.0, 1.0, 0.0, 0.5)),
        (2, segment(&path, 0.0, 1.0, 0.0, 0.5)),
    ]);
    let summed = peak(&render(&mut both, 1.0), 0.1, 0.9);
    assert!((summed - 0.5).abs() < 0.01, "{summed}");
}

#[test]
fn fades_ramp_linearly_and_overlong_fades_meet_in_proportion() {
    let path = tone_file("tone.wav", 0.0, &["-c:a", "pcm_s16le"]);
    let faded = |fade_in: f64, fade_out: f64| {
        let mut clip = segment(&path, 1.0, 2.0, 0.0, 1.0);
        (clip.fade_in, clip.fade_out) = (fade_in, fade_out);
        let mut engine = playing(&[(1, clip)]);
        render(&mut engine, 3.0)
    };
    // 0.5 s in, 1 s out on a 2 s clip at timeline 1 s: the ramps are
    // measured on the timeline, not from the source's start.
    let mix = faded(0.5, 1.0);
    let at = |t: f64| peak(&mix, t - 0.005, t + 0.005);
    assert!(at(1.002) < 0.01, "starts silent: {}", at(1.002));
    assert!((at(1.25) - 0.25).abs() < 0.02, "mid fade-in: {}", at(1.25));
    assert!((at(1.75) - 0.5).abs() < 0.01, "full level: {}", at(1.75));
    assert!((at(2.5) - 0.25).abs() < 0.02, "mid fade-out: {}", at(2.5));
    assert!(at(2.995) < 0.01, "ends silent: {}", at(2.995));

    // 3 s + 1 s of fades on a 2 s clip scale to 1.5 s + 0.5 s.
    let mix = faded(3.0, 1.0);
    let at = |t: f64| peak(&mix, t - 0.005, t + 0.005);
    assert!(
        (at(1.75) - 0.25).abs() < 0.02,
        "scaled fade-in: {}",
        at(1.75)
    );
    assert!(
        (at(2.5) - 0.5).abs() < 0.02,
        "fades meet at 1.5 s: {}",
        at(2.5)
    );
    assert!(
        (at(2.75) - 0.25).abs() < 0.02,
        "scaled fade-out: {}",
        at(2.75)
    );
}

#[test]
fn ducking_dips_under_speech_with_ramps_and_updates_without_restarting() {
    let path = tone_file("tone.wav", 0.0, &["-c:a", "pcm_s16le"]);
    // A clip at timeline 1 s ducks over its own seconds 1.0..1.5.
    let mut clip = segment(&path, 1.0, 3.0, 0.0, 1.0);
    clip.duck = vec![(1.0, 1.5)];
    let mut engine = playing(&[(1, clip.clone())]);
    let mix = render(&mut engine, 4.0);
    let at = |t: f64| peak(&mix, t - 0.005, t + 0.005);
    assert!(
        (at(1.5) - 0.5).abs() < 0.01,
        "full before the ramp: {}",
        at(1.5)
    );
    assert!(
        (at(1.85) - 0.3125).abs() < 0.02,
        "halfway down: {}",
        at(1.85)
    );
    assert!(
        (at(2.25) - 0.125).abs() < 0.01,
        "ducked to a quarter: {}",
        at(2.25)
    );
    assert!((at(3.0) - 0.5).abs() < 0.01, "recovered: {}", at(3.0));

    // New ranges reach a playing voice without restarting it.
    let mut plain = clip.clone();
    plain.duck.clear();
    let mut engine = playing(&[(1, plain)]);
    let head = render(&mut engine, 1.5);
    assert!((peak(&head, 1.1, 1.5) - 0.5).abs() < 0.01);
    clip.duck = vec![(1.5, 2.5)];
    engine.set_voices(&[(1, clip)]);
    let tail = render(&mut engine, 2.0);
    let ducked = peak(&tail, 1.25, 1.75);
    assert!(
        (ducked - 0.125).abs() < 0.01,
        "timeline 2.75 is ducked: {ducked}"
    );
}

#[test]
fn picture_limit_holds_the_clock_and_audio() {
    let path = tone_file("tone.wav", 0.0, &["-c:a", "pcm_s16le"]);
    let mut engine = AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(0.5);
    engine.set_running(true);
    engine.set_voices(&[(1, segment(&path, 0.0, 2.0, 0.0, 1.0))]);
    engine.advance(1.0);
    assert!((engine.clock() - 0.5).abs() < 1e-9, "{}", engine.clock());
    assert!(engine.render(512).is_empty(), "rendered past the limit");

    engine.set_running(false);
    engine.set_limit(2.0);
    assert!(engine.render(512).is_empty(), "paused engine advanced");
    engine.set_running(true);
    engine.advance(0.25);
    assert!((engine.clock() - 0.75).abs() < 1e-9, "{}", engine.clock());
}

#[test]
fn seek_drops_voices_and_moves_the_clock() {
    let path = tone_file("tone.wav", 0.0, &["-c:a", "pcm_s16le"]);
    let mut engine = playing(&[(1, segment(&path, 0.0, 4.0, 0.0, 1.0))]);
    engine.advance(0.5);
    engine.seek(3.0);
    assert_eq!(engine.clock(), 3.0);
    assert!(engine.voice_ids().is_empty());
    let mix = render(&mut engine, 0.25);
    assert_eq!(peak(&mix, 0.0, 0.25), 0.0, "dropped voice still audible");
    assert!(engine.take_error().is_none());
}

#[test]
fn missing_file_reports_an_error_without_holding_the_clock() {
    let mut engine = playing(&[(
        1,
        segment(Path::new("/nonexistent.wav"), 0.0, 1.0, 0.0, 1.0),
    )]);
    render(&mut engine, 0.5);
    assert!(engine.take_error().is_some());
}

#[test]
fn null_output_paces_the_clock_in_real_time() {
    let engine = AudioEngine::null();
    let mut engine = engine;
    engine.seek(1.0);
    engine.set_limit(100.0);
    let started = Instant::now();
    engine.set_running(true);
    std::thread::sleep(Duration::from_millis(400));
    let elapsed = started.elapsed().as_secs_f64();
    let advanced = engine.clock() - 1.0;
    assert!(
        (advanced - elapsed).abs() < 0.08,
        "advanced {advanced} in {elapsed}"
    );
}

#[test]
fn preview_loudness_applies_gain_then_limits_peaks() {
    let path = tone_file("tone.wav", 0.0, &["-c:a", "pcm_s16le"]);
    // Quieter: plain gain, no limiting below the ceiling.
    let mut engine = playing(&[(1, segment(&path, 0.0, 1.0, 0.0, 1.0))]);
    engine.set_loudness(Some(0.5));
    let quiet = peak(&render(&mut engine, 1.0), 0.1, 0.9);
    assert!((quiet - 0.25).abs() < 0.01, "{quiet}");
    // Louder: 0.5 x 3 would be 1.5; the limiter holds it at 0.8.
    let mut engine = playing(&[(1, segment(&path, 0.0, 2.0, 0.0, 1.0))]);
    engine.set_loudness(Some(3.0));
    let mix = render(&mut engine, 1.0);
    let held = peak(&mix, 0.1, 0.9);
    assert!(held <= 0.8 + 1e-4 && held > 0.75, "limited to {held}");
    // Turning it off returns the plain mix.
    engine.set_loudness(None);
    let plain = peak(&render(&mut engine, 0.5), 0.1, 0.4);
    assert!((plain - 0.5).abs() < 0.01, "{plain}");
}

#[test]
fn offline_mix_measurement_matches_ffmpeg_ebur128() {
    // Two overlapping clips with a gain change: the in-process meter over
    // the preview mix must agree with FFmpeg's R128 meter on the same mix.
    let path = tone_file("tone.m4a", 0.0, &["-c:a", "aac", "-b:a", "192k"]);
    let segments = [
        (1, segment(&path, 0.0, 3.0, 0.5, 0.4)),
        (2, segment(&path, 1.0, 2.5, 0.0, 0.2)),
    ];
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let ours = openreel_media::measure_mix(&segments, 3.5, &cancel)
        .unwrap()
        .integrated(0.0..3.5)
        .unwrap();
    // The same mix rendered by FFmpeg: delay, volume, sum.
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(&path)
        .arg("-i")
        .arg(&path)
        .args([
            "-filter_complex",
            "[0:a]atrim=0.5:3.5,asetpts=PTS-STARTPTS,aresample=48000,volume=0.4[a];\
             [1:a]atrim=0:2.5,asetpts=PTS-STARTPTS,aresample=48000,volume=0.2,adelay=1000|1000[b];\
             [a][b]amix=inputs=2:duration=longest:normalize=0,apad=whole_dur=3.5,ebur128",
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("ffmpeg");
    let log = String::from_utf8_lossy(&out.stderr);
    let summary = &log[log.rfind("Summary:").expect("summary")..];
    let theirs: f64 = summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("I:"))
        .unwrap()
        .trim()
        .trim_end_matches("LUFS")
        .trim()
        .parse()
        .unwrap();
    eprintln!("in-process {ours:.2} LUFS, ffmpeg {theirs:.2} LUFS");
    // ebur128 prints one decimal.
    assert!((ours - theirs).abs() < 0.2, "{ours} vs {theirs}");
}

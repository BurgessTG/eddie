//! Public export regressions, checked by independently decoding every output
//! frame with FFmpeg. No native still/preview decoder is used as the oracle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;

use openreel_engine::{
    Background, ClipKind, Format, Property, Timeline, TitleStyle, Transform, Transition,
    TransitionKind,
};
use openreel_media::{
    export_timeline, probe, AudioFormat, ExportKind, ExportOptions, ExportSettings, GifSize,
    Quality, Resolution, VideoDecoder,
};

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "openreel_export_{}_{}",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("temp dir");
        Self(dir)
    }

    fn video(&self, name: &str, source: &str, filter: &str, tone: Option<u32>) -> String {
        let path = self.0.join(name);
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i", source]);
        if let Some(hz) = tone {
            cmd.args([
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency={hz}:sample_rate=48000"),
                "-shortest",
            ]);
        }
        cmd.args([
            "-vf",
            filter,
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-crf",
            "0",
            "-g",
            "120",
            "-sc_threshold",
            "0",
            "-threads:v",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(&path);
        run(&mut cmd);
        path.to_str().unwrap().to_owned()
    }

    fn indices(&self, name: &str, rate: &str) -> String {
        self.video(
            name,
            &format!("nullsrc=s=64x36:r={rate}:d=1"),
            "geq=lum='24+6*N+16*mod(floor(X/8)+floor(Y/6),2)':cb=128:cr=128",
            None,
        )
    }

    fn color(&self, name: &str, color: &str, tone: Option<u32>) -> String {
        self.video(
            name,
            &format!("color=c={color}:s=320x180:r=30:d=3"),
            "null",
            tone,
        )
    }

    fn export(&self, name: &str, timeline: &Timeline) -> PathBuf {
        self.export_with(name, timeline, &ExportOptions::default())
    }

    fn export_with(&self, name: &str, timeline: &Timeline, options: &ExportOptions) -> PathBuf {
        let mut infos = HashMap::new();
        for path in timeline
            .clips
            .iter()
            .filter_map(|clip| clip.source.as_ref())
        {
            infos
                .entry(path.clone())
                .or_insert_with(|| probe(path).expect("probe fixture"));
        }
        let path = self.0.join(name);
        let (tx, rx) = channel();
        export_timeline(timeline, &infos, &path, options, tx).expect("public export_timeline");
        let progress: Vec<_> = rx.try_iter().collect();
        assert_eq!(progress.last(), Some(&1.0));
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(cmd: &mut Command) -> Output {
    let out = cmd.output().expect("run FFmpeg tool");
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn frames(path: &Path) -> Vec<Vec<u8>> {
    let info = probe(path.to_str().unwrap()).expect("probe decoded file");
    let out = run(Command::new("ffmpeg").args([
        "-v",
        "error",
        "-i",
        path.to_str().unwrap(),
        "-map",
        "0:v:0",
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-",
    ]));
    let bytes = info.width as usize * info.height as usize * 3;
    assert_eq!(out.stdout.len() % bytes, 0, "complete RGB frames");
    out.stdout.chunks_exact(bytes).map(<[u8]>::to_vec).collect()
}

fn assert_frames(label: &str, actual: &[Vec<u8>], expected: &[&[u8]]) {
    assert_eq!(actual.len(), expected.len(), "{label}: frame count");
    let errors: Vec<_> = actual
        .iter()
        .zip(expected)
        .map(|(a, b)| {
            assert_eq!(a.len(), b.len(), "{label}: frame size");
            a.iter()
                .zip(b.iter())
                .map(|(&x, &y)| x.abs_diff(y) as f64)
                .sum::<f64>()
                / a.len() as f64
        })
        .collect();
    eprintln!(
        "{label}: {} frames, full-frame RGB mean absolute errors={errors:?}",
        actual.len()
    );
    for (index, error) in errors.iter().enumerate() {
        assert!(
            *error < 2.5,
            "{label}: frame {index}, full-frame RGB error {error} (limit 2.5)"
        );
    }
}

fn video(
    timeline: &mut Timeline,
    path: &str,
    track: usize,
    start: f64,
    duration: f64,
    source_in: f64,
) -> u64 {
    timeline.add_media_clip(
        "picture",
        ClipKind::Video,
        track,
        start,
        duration,
        source_in,
        path,
    )
}

fn extend(timeline: &mut Timeline, end: f64) {
    // A source-less audio clip extends the canvas without rendering a title
    // or supplying another picture. The public model permits generated clips.
    timeline.add_clip("tail", ClipKind::Audio, 0, 0.0, end, 0.0);
}

fn stream_json(path: &Path) -> serde_json::Value {
    let out = run(Command::new("ffprobe").args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=time_base,r_frame_rate:frame=best_effort_timestamp_time",
        "-of",
        "json",
        path.to_str().unwrap(),
    ]));
    serde_json::from_slice(&out.stdout).expect("timestamp JSON")
}

#[test]
fn source_in_preserves_mp4_frame_indices_and_mkv_control() {
    let fixture = Fixture::new();
    for (name, source_in, time_base) in [
        ("indices.mp4", 2.0 / 30.0, "1/15360"),
        // MKV stores frame 2 at .067, not at the ideal CFR time 2/30.
        ("indices.mkv", 0.067, "1/1000"),
    ] {
        let source = fixture.indices(name, "30");
        assert_eq!(
            stream_json(Path::new(&source))["streams"][0]["time_base"],
            time_base
        );
        let original = frames(Path::new(&source));
        let mut timeline = Timeline::new(30.0);
        video(&mut timeline, &source, 0, 0.0, 3.0 / 30.0, source_in);
        let exported = fixture.export(&format!("{name}.export.mp4"), &timeline);
        assert_frames(
            name,
            &frames(&exported),
            &[&original[2], &original[3], &original[4]],
        );
    }
}

#[test]
fn arbitrary_source_in_holds_the_covering_frame() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices.mp4", "30");
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &source, 0, 0.0, 3.0 / 30.0, 2.4 / 30.0);
    let exported = fixture.export("arbitrary.mp4", &timeline);
    assert_frames(
        "source in 2.4/30",
        &frames(&exported),
        &[&original[2], &original[3], &original[4]],
    );
}

#[test]
fn scene_placement_preserves_source_playback_across_frame_rates() {
    let fixture = Fixture::new();
    let source = fixture.video(
        "scenes.mp4",
        "nullsrc=s=80x48:r=120:d=1",
        "geq=lum='40+160*mod(floor(N/pow(2,floor(X/8))),2)':cb=128:cr=128",
        Some(440),
    );
    let original = frames(Path::new(&source));
    assert_eq!(original.len(), 120);
    let boundaries = [0, 1, 2, 17, 18, 19, 61, 97, 120];
    for fps in [30, 120] {
        let mut timeline = Timeline::new(fps as f64);
        for pair in boundaries.windows(2) {
            video(
                &mut timeline,
                &source,
                0,
                pair[0] as f64 / 120.0,
                (pair[1] - pair[0]) as f64 / 120.0,
                pair[0] as f64 / 120.0,
            );
        }
        let expected: Vec<_> = (0..fps)
            .map(|n| original[n * 120 / fps].as_slice())
            .collect();
        let exported = fixture.export(&format!("scenes-{fps}.mp4"), &timeline);
        assert_frames(
            &format!("120 fps scenes at {fps} fps"),
            &frames(&exported),
            &expected,
        );
        assert!(tone_amplitude(&exported, 440.0) > 0.08);
    }
}

fn audio_samples(path: &Path) -> Vec<f32> {
    let pcm = run(Command::new("ffmpeg").args([
        "-v",
        "error",
        "-i",
        path.to_str().unwrap(),
        "-vn",
        "-ar",
        "48000",
        "-ac",
        "1",
        "-f",
        "f32le",
        "-",
    ]));
    assert_eq!(pcm.stdout.len() % 4, 0, "complete audio samples");
    pcm.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect()
}

fn assert_export_end(fps: f64, end: f64, count: usize, with_audio: bool) {
    let fixture = Fixture::new();
    let source = fixture.video(
        "end.mp4",
        "nullsrc=s=80x48:r=60:d=1.5",
        "geq=lum='40+160*mod(floor(N/pow(2,floor(X/8))),2)':cb=128:cr=128",
        with_audio.then_some(440),
    );
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(fps);
    let boundaries: Vec<_> = [0.0, 1.0 / 3.0, 49.0 / 60.0]
        .into_iter()
        .filter(|time| *time < end)
        .chain([end])
        .collect();
    for pair in boundaries.windows(2) {
        video(
            &mut timeline,
            &source,
            0,
            pair[0],
            pair[1] - pair[0],
            pair[0],
        );
    }
    let exported = fixture.export("end-export.mp4", &timeline);
    let actual = frames(&exported);
    let samples = audio_samples(&exported);
    let expected_samples = (end * 48000.0).round() as usize;
    let label = format!("{fps} fps, end={end}, audio={with_audio}");
    eprintln!(
        "{label}: decoded {} of {count} expected frames, {} of {expected_samples} expected audio samples",
        actual.len(),
        samples.len()
    );
    let expected: Vec<_> = (0..count)
        .map(|n| original[(n as f64 * 60.0 / fps + 1e-10).floor() as usize].as_slice())
        .collect();
    assert_frames(&label, &actual, &expected);
    assert_eq!(samples.len(), expected_samples, "{label}: audio duration");
    if with_audio {
        let tail_peak = samples[samples.len() - 48..]
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        assert!(tail_peak > 0.02, "{label}: final millisecond is audible");
    } else {
        assert!(samples.iter().all(|sample| sample.abs() < 0.0001));
    }
    let json = stream_json(&exported);
    assert_eq!(json["frames"].as_array().unwrap().len(), count);
    for (n, frame) in json["frames"].as_array().unwrap().iter().enumerate() {
        let pts: f64 = frame["best_effort_timestamp_time"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((pts - n as f64 / fps).abs() < 0.000001);
        assert!(pts < end, "{label}: frame {n} is outside the timeline");
    }
}

#[test]
fn off_grid_end_exports_the_final_active_frame_without_extending_audio() {
    for with_audio in [true, false] {
        assert_export_end(30.0, 1.235, 38, with_audio);
    }
}

#[test]
fn exact_grid_end_excludes_the_end_sample() {
    for (fps, count, source_frame) in [
        (30.0, 37, 0),
        (30000.0 / 1001.0, 37, 0),
        (60000.0 / 1001.0, 74, 0),
        (24000.0 / 1001.0, 6, 947),
        (24000.0 / 1001.0, 1, 953),
        (24000.0 / 1001.0, 6, 100_000),
    ] {
        // Curated durations subtract source timestamps, which can have much
        // more floating-point cancellation error than a duration made at zero.
        let end = (source_frame + count) as f64 / fps - source_frame as f64 / fps;
        assert_export_end(fps, end, count, true);
    }
}

#[test]
fn fractional_rate_off_grid_end_exports_the_final_active_frame() {
    for (fps, count) in [(30000.0 / 1001.0, 38), (60000.0 / 1001.0, 75)] {
        assert_export_end(fps, 1.235, count, true);
    }
    let fps = 24000.0 / 1001.0;
    // Six 48 kHz samples keep audio expectations exact on both sides too.
    assert_export_end(fps, 6.0 / fps - 0.000125, 6, true);
    assert_export_end(fps, 6.0 / fps + 0.000125, 7, true);
}

#[test]
fn off_grid_end_limits_delayed_audio_on_the_sample_clock() {
    let fixture = Fixture::new();
    let source = fixture.color("delayed-tone.mp4", "red", Some(440));
    for contributions in [1, 2] {
        let mut timeline = Timeline::new(30.0);
        let picture = video(&mut timeline, &source, 0, 0.0, 1.235, 0.0);
        timeline.clip_mut(picture).unwrap().embedded_audio = false;
        for track in 0..contributions {
            timeline.add_media_clip(
                "sound",
                ClipKind::Audio,
                track,
                0.0006,
                1.235 - 0.0006,
                0.0,
                &source,
            );
        }
        let exported = fixture.export(&format!("delayed-{contributions}.mp4"), &timeline);
        let samples = audio_samples(&exported);
        eprintln!(
            "{contributions} delayed audio contributions: decoded {} of 59280 expected samples",
            samples.len()
        );
        assert_eq!(samples.len(), 59280, "rounded delays must not extend audio");
        let tail_peak = samples[samples.len() - 48..]
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        assert!(tail_peak > 0.02, "final millisecond is audible");
    }
}

/// A clip on the frame grid is rarely on the millisecond grid: 1/30 s is
/// 1600 samples but 33.33 ms. Its sound starts on the sample the preview
/// uses, not up to half a millisecond away.
#[test]
fn audio_starts_on_its_exact_sample_not_the_nearest_millisecond() {
    let fixture = Fixture::new();
    let source = fixture.color("tone.mp4", "red", Some(440));
    let omega = std::f64::consts::TAU * 440.0 / 48000.0;
    let period = 48000.0 / 440.0;
    // How far the exported tone lags a sine from output sample 0, in samples
    // modulo the tone's period. Phase survives the AAC encode; an onset does
    // not, to the sample.
    let lag = |name: &str, start: f64| {
        let mut timeline = Timeline::new(30.0);
        let picture = video(&mut timeline, &source, 0, 0.0, 2.0, 0.0);
        timeline.clip_mut(picture).unwrap().embedded_audio = false;
        timeline.add_media_clip("sound", ClipKind::Audio, 1, start, 1.5, 0.0, &source);
        let samples = audio_samples(&fixture.export(name, &timeline));
        let (mut sin, mut cos) = (0.0, 0.0);
        for (n, sample) in samples.iter().enumerate().take(57600).skip(24000) {
            sin += f64::from(*sample) * (omega * n as f64).sin();
            cos += f64::from(*sample) * (omega * n as f64).cos();
        }
        (-cos).atan2(sin).rem_euclid(std::f64::consts::TAU) / omega
    };
    let shift = (lag("late.mp4", 1.0 / 30.0) - lag("first.mp4", 0.0)).rem_euclid(period);
    // 1600 samples is 14 periods and 72.7 samples; 33 ms would leave 56.7.
    let expected = 1600.0 - 14.0 * period;
    assert!(
        (shift - expected).abs() < 2.0,
        "the tone starts {shift:.1} samples into its period, not {expected:.1}"
    );
}

#[test]
fn gaps_and_adjacent_cuts_use_exact_output_frame_offsets() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices.mp4", "30");
    let original = frames(Path::new(&source));
    let black = vec![0; original[0].len()];
    let mut timeline = Timeline::new(30.0);
    video(
        &mut timeline,
        &source,
        0,
        1.0 / 30.0,
        2.0 / 30.0,
        2.0 / 30.0,
    );
    video(
        &mut timeline,
        &source,
        0,
        3.0 / 30.0,
        1.0 / 30.0,
        9.0 / 30.0,
    );
    video(
        &mut timeline,
        &source,
        0,
        5.0 / 30.0,
        2.0 / 30.0,
        5.0 / 30.0,
    );
    extend(&mut timeline, 8.0 / 30.0);
    let exported = fixture.export("gaps.mp4", &timeline);
    assert_frames(
        "leading/interior/trailing gaps and adjacent cut",
        &frames(&exported),
        &[
            &black,
            &original[2],
            &original[3],
            &original[9],
            &black,
            &original[5],
            &original[6],
            &black,
        ],
    );
}

#[test]
fn fractional_rate_keeps_frame_offsets_content_and_cadence() {
    let fixture = Fixture::new();
    let source = fixture.indices("ntsc.mp4", "30000/1001");
    let original = frames(Path::new(&source));
    let black = vec![0; original[0].len()];
    let fps = 30000.0 / 1001.0;
    let mut timeline = Timeline::new(fps);
    video(&mut timeline, &source, 0, 1.0 / fps, 3.0 / fps, 2.0 / fps);
    extend(&mut timeline, 6.0 / fps);
    let exported = fixture.export("fractional.mp4", &timeline);
    assert_frames(
        "30000/1001",
        &frames(&exported),
        &[
            &black,
            &original[2],
            &original[3],
            &original[4],
            &black,
            &black,
        ],
    );
    let json = stream_json(&exported);
    assert_eq!(json["streams"][0]["r_frame_rate"], "30000/1001");
    for (i, frame) in json["frames"].as_array().unwrap().iter().enumerate() {
        let pts: f64 = frame["best_effort_timestamp_time"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            (pts - i as f64 / fps).abs() < 0.000001,
            "frame {i}: PTS {pts}"
        );
    }
    eprintln!("fractional output stream and frame timestamps: {json}");
}

#[test]
fn rate_conversion_samples_the_covering_frame_not_the_nearest_frame() {
    let fixture = Fixture::new();
    for (source_rate, source_fps, output_fps, source_in, count) in [
        ("30", 30.0, 10.0, 2.0 / 30.0, 5),
        ("10", 10.0, 30.0, 0.04, 10),
        ("24", 24.0, 30000.0 / 1001.0, 2.25 / 24.0, 10),
    ] {
        let source = fixture.indices(&format!("rate-{source_rate}.mp4"), source_rate);
        let original = frames(Path::new(&source));
        let mut timeline = Timeline::new(output_fps);
        video(
            &mut timeline,
            &source,
            0,
            0.0,
            count as f64 / output_fps,
            source_in,
        );
        let exported = fixture.export(&format!("converted-{source_rate}.mp4"), &timeline);
        let indices: Vec<_> = (0..count)
            .map(|n| ((source_in + n as f64 / output_fps) * source_fps + 1e-10).floor() as usize)
            .collect();
        let expected: Vec<_> = indices.iter().map(|&n| original[n].as_slice()).collect();
        eprintln!("{source_rate} -> {output_fps}: expected source frames {indices:?}");
        assert_frames("rate conversion", &frames(&exported), &expected);
    }
}

#[test]
fn source_in_is_relative_to_a_nonzero_container_origin() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices.mp4", "30");
    let offset = fixture.0.join("offset.mp4");
    run(Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-i",
            &source,
            "-c",
            "copy",
            "-output_ts_offset",
            "3.25",
        ])
        .arg(&offset));
    assert_eq!(
        stream_json(&offset)["frames"][0]["best_effort_timestamp_time"],
        "3.250000"
    );
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(30.0);
    video(
        &mut timeline,
        offset.to_str().unwrap(),
        0,
        0.0,
        3.0 / 30.0,
        2.0 / 30.0,
    );
    let exported = fixture.export("normalized.mp4", &timeline);
    assert_frames(
        "normalized source origin",
        &frames(&exported),
        &[&original[2], &original[3], &original[4]],
    );
}

#[test]
fn vfr_in_point_uses_presentation_intervals_and_holds_at_eof() {
    let fixture = Fixture::new();
    let source = fixture.video(
        "vfr.mp4",
        "nullsrc=s=64x36:r=10:d=3",
        "geq=lum='24+6*N':cb=128:cr=128,select='eq(n,0)+gte(n,20)'",
        None,
    );
    let json = stream_json(Path::new(&source));
    assert_eq!(json["frames"][1]["best_effort_timestamp_time"], "2.000000");
    let original = frames(Path::new(&source));
    assert_eq!(original.len(), 11);
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 1.1, 1.5);
    let exported = fixture.export("vfr-in-point.mp4", &timeline);
    assert_frames(
        "VFR hold and exact next PTS",
        &frames(&exported),
        &[
            &original[0],
            &original[0],
            &original[0],
            &original[0],
            &original[0],
            &original[1],
            &original[2],
            &original[3],
            &original[4],
            &original[5],
            &original[6],
        ],
    );
    let mut tail = Timeline::new(10.0);
    video(&mut tail, &source, 0, 0.0, 0.3, 2.9);
    let exported = fixture.export("vfr-tail.mp4", &tail);
    assert_frames(
        "VFR final frame hold",
        &frames(&exported),
        &[original[10].as_slice(); 3],
    );

    let source = fixture.indices("fast-tail.mp4", "30");
    let original = frames(Path::new(&source));
    assert_eq!(original.len(), 30);
    let longer_audio = fixture.0.join("longer-audio.mp4");
    run(Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i", &source])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1.2",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
        ])
        .arg(&longer_audio));
    let source = longer_audio.to_str().unwrap();
    assert!((probe(source).unwrap().duration - 1.2).abs() < 0.001);
    let mut tail = Timeline::new(10.0);
    video(&mut tail, source, 0, 0.0, 0.4, 0.8);
    let exported = fixture.export("last-source-frame.mp4", &tail);
    assert_frames(
        "Audio tail holds the final source frame, not the last sampled frame",
        &frames(&exported),
        &[&original[24], &original[27], &original[29], &original[29]],
    );
}

#[test]
fn mixed_color_matrices_and_ranges_preserve_rgb_through_export() {
    let fixture = Fixture::new();
    let mut timeline = Timeline::new(10.0);
    let mut expected = Vec::new();
    // Untagged sources are BT.601 limited range.
    for (index, (matrix, range)) in [
        ("bt709", "tv"),
        ("bt470bg", "tv"),
        ("bt709", "pc"),
        ("unknown", "unknown"),
    ]
    .into_iter()
    .enumerate()
    {
        let (encoded, encoded_range) = match (matrix, range) {
            ("unknown", "unknown") => ("bt601", "tv"),
            _ => (matrix, range),
        };
        let source = fixture.video(
            &format!("{matrix}-{range}.mp4"),
            "color=c=0xD14C29:s=64x36:r=10:d=0.5",
            &format!(
                "drawbox=x=32:y=0:w=32:h=36:color=0x2369BD:t=fill,\
                 scale=in_color_matrix=bt601:out_color_matrix={encoded}:out_range={encoded_range},\
                 setparams=colorspace={matrix}:range={range}"
            ),
            None,
        );
        let metadata = run(Command::new("ffprobe").args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=color_space,color_range",
            "-of",
            "json",
            &source,
        ]));
        let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
        let tagged = metadata["streams"][0]["color_space"].as_str();
        assert_eq!(tagged.unwrap_or("unknown"), matrix);
        let tagged = metadata["streams"][0]["color_range"].as_str();
        assert_eq!(tagged.unwrap_or("unknown"), range);
        let original = frames(Path::new(&source));
        for (offset, opacity) in [(0.0, 1.0), (0.2, 0.5)] {
            let id = video(
                &mut timeline,
                &source,
                0,
                index as f64 * 0.4 + offset,
                0.2,
                0.0,
            );
            timeline.clip_mut(id).unwrap().opacity = opacity;
            let picture: Vec<_> = original[0]
                .iter()
                .map(|&value| (value as f32 * opacity).round() as u8)
                .collect();
            expected.extend([picture.clone(), picture]);
        }
    }
    let exported = fixture.export("mixed-colors.mp4", &timeline);
    let references: Vec<_> = expected.iter().map(Vec::as_slice).collect();
    assert_frames(
        "mixed SDR matrices/ranges, full and half opacity",
        &frames(&exported),
        &references,
    );
}

#[test]
fn textured_bt601_sources_keep_their_average_color_through_export() {
    // Phone footage is often BT.601 (limited or full range). Converting its
    // subsampled chroma straight to BT.709 used to darken green by ~1.5
    // levels on textured pictures; flat color fields hide the bias.
    let fixture = Fixture::new();
    for range in ["tv", "pc"] {
        let source = fixture.video(
            &format!("bt601-{range}.mp4"),
            // seed controls geometry, not the default random colors. Pin both
            // endpoints so each run and both ranges exercise the same picture.
            "gradients=s=320x180:r=10:d=0.3:seed=7:c0=0xD14C29:c1=0x2369BD",
            &format!(
                "scale=out_color_matrix=bt601:out_range={range},format=yuv420p,\
                 setparams=colorspace=smpte170m:range={range}"
            ),
            None,
        );
        let mut timeline = Timeline::new(10.0);
        video(&mut timeline, &source, 0, 0.0, 0.3, 0.0);
        // Decode both with exact chroma interpolation so the reference
        // conversion does not add a matrix-dependent bias of its own.
        let accurate = |path: &Path| {
            let out = run(Command::new("ffmpeg").args([
                "-v",
                "error",
                "-i",
                path.to_str().unwrap(),
                "-vf",
                "scale=flags=accurate_rnd+full_chroma_int+full_chroma_inp,format=rgb24",
                "-f",
                "rawvideo",
                "-",
            ]));
            out.stdout
                .as_chunks::<{ 320 * 180 * 3 }>()
                .0
                .iter()
                .map(|frame| frame.to_vec())
                .collect::<Vec<_>>()
        };
        let exported = accurate(&fixture.export(&format!("bt601-{range}-out.mp4"), &timeline));
        let original = accurate(Path::new(&source));
        assert_eq!(exported.len(), original.len());
        for (index, (actual, expected)) in exported.iter().zip(&original).enumerate() {
            let mut bias = [0.0_f64; 3];
            for (pixel, reference) in actual
                .as_chunks::<3>()
                .0
                .iter()
                .zip(expected.as_chunks::<3>().0)
            {
                for channel in 0..3 {
                    bias[channel] += pixel[channel] as f64 - reference[channel] as f64;
                }
            }
            let pixels = (actual.len() / 3) as f64;
            let bias = bias.map(|sum| sum / pixels);
            eprintln!("BT.601 {range} frame {index}: mean RGB bias {bias:?}");
            assert!(
                // Allow lossy encode/round-trip error, but not the green shift
                // from BT.601 to BT.709 without full chroma interpolation.
                bias.iter().all(|value| value.abs() < 0.9),
                "BT.601 {range} frame {index}: mean RGB bias {bias:?} (limit 0.9)"
            );
        }
    }
}

#[test]
fn lower_track_wins_even_when_a_higher_track_starts_later() {
    let fixture = Fixture::new();
    let red = fixture.color("red.mp4", "red", None);
    let blue = fixture.color("blue.mp4", "blue", None);
    let original = frames(Path::new(&red));
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &red, 0, 0.0, 3.0, 0.0);
    video(&mut timeline, &blue, 1, 1.0, 1.0, 0.0);
    let exported = fixture.export("lanes.mp4", &timeline);
    assert_frames(
        "lowest lane, including t=1.5",
        &frames(&exported),
        &[original[0].as_slice(); 30],
    );
}

#[test]
fn same_track_ties_use_insertion_order_not_start_order() {
    let fixture = Fixture::new();
    let red = fixture.color("red.mp4", "red", None);
    let blue = fixture.color("blue.mp4", "blue", None);
    let original = frames(Path::new(&red));
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &red, 0, 0.0, 0.4, 0.0);
    video(&mut timeline, &blue, 0, 0.1, 0.2, 0.0);
    let exported = fixture.export("ties.mp4", &timeline);
    assert_frames(
        "same-track insertion order",
        &frames(&exported),
        &[original[0].as_slice(); 4],
    );
}

/// Red at `opacity` on lane 0 over blue on lane 1: the blue shows through.
fn opacity_over_another_lane(opacity: f32) {
    let fixture = Fixture::new();
    let red = fixture.color("red.mp4", "red", None);
    let blue = fixture.color("blue.mp4", "blue", None);
    let mut timeline = Timeline::new(10.0);
    let id = video(&mut timeline, &red, 0, 0.0, 0.3, 0.0);
    timeline.clip_mut(id).unwrap().opacity = opacity;
    video(&mut timeline, &blue, 1, 0.0, 0.3, 0.0);
    let expected: Vec<_> = frames(Path::new(&red))[0]
        .iter()
        .zip(&frames(Path::new(&blue))[0])
        .map(|(&red, &blue)| (red as f32 * opacity + blue as f32 * (1.0 - opacity)).round() as u8)
        .collect();
    let exported = fixture.export("opacity.mp4", &timeline);
    assert_frames(
        &format!("opacity={opacity} over blue"),
        &frames(&exported),
        &[expected.as_slice(); 3],
    );
}

#[test]
fn zero_opacity_video_shows_the_lane_under_it() {
    opacity_over_another_lane(0.0);
}

#[test]
fn partial_opacity_video_shows_the_lane_under_it_through_it() {
    opacity_over_another_lane(0.5);
}

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 128, 0];
const BLUE: [u8; 3] = [0, 0, 255];

/// Each lane draws over the lanes under it: a half-size red picture on
/// lane 1 over blue on lane 2, and over both a PNG whose left 60% is
/// green and the rest clear. The PNG covers part of the red picture; the
/// red and blue show through where it is clear.
#[test]
fn picture_in_picture_draws_each_lane_over_the_ones_under_it() {
    let fixture = Fixture::new();
    let blue = fixture.color("blue.mp4", "blue", None);
    let red = fixture.color("red.mp4", "red", None);
    let png = fixture.0.join("half.png");
    run(Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("color=c=green:s=320x180,format=rgba,geq=r=0:g=128:b=0:a='255*lt(X,192)'")
        .args(["-frames:v", "1"])
        .arg(&png));
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &blue, 2, 0.0, 0.3, 0.0);
    let pip = video(&mut timeline, &red, 1, 0.0, 0.3, 0.0);
    timeline.clip_mut(pip).unwrap().transform = Transform {
        scale: 0.5,
        x: 0.25,
        ..Transform::default()
    };
    let sticker = video(&mut timeline, png.to_str().unwrap(), 0, 0.0, 0.3, 0.0);
    timeline.clip_mut(sticker).unwrap().photo = true;
    let exported = fixture.export("pip.mp4", &timeline);
    // The red picture fills x 160-320, y 45-135.
    let colors = colors_at(
        &frames(&exported)[1],
        320,
        &[(80, 90), (176, 90), (240, 90), (240, 20), (280, 160)],
    );
    assert_colors("lanes", &colors, &[GREEN, GREEN, RED, BLUE, BLUE]);
}

/// Between two half-size pictures on lane 0 over blue on lane 1, halfway
/// through each kind: a wipe moves only the pictures, so the blue around
/// them stays; a mix crossfades whole frames, which here differ only in
/// the picture; a dip veils everything under the lane too.
#[test]
fn a_transition_over_another_lane_keeps_or_veils_what_is_under_it() {
    let fixture = Fixture::new();
    let blue = fixture.color("blue.mp4", "blue", None);
    let red = fixture.color("red.mp4", "red", None);
    let green = fixture.color("green.mp4", "green", None);
    let half = |kind: TransitionKind| {
        let mut timeline = Timeline::new(10.0);
        video(&mut timeline, &blue, 1, 0.0, 2.0, 0.0);
        let a = video(&mut timeline, &red, 0, 0.0, 1.0, 0.0);
        let b = video(&mut timeline, &green, 0, 1.0, 1.0, 1.0);
        for id in [a, b] {
            timeline.clip_mut(id).unwrap().transform.scale = 0.5;
        }
        timeline.clip_mut(b).unwrap().transition = Some(Transition::new(kind, 1.0));
        let exported = fixture.export(&format!("{}.mp4", kind.as_str()), &timeline);
        // The pictures fill x 80-240, y 45-135; frame 10 is the cut.
        colors_at(
            &frames(&exported)[10],
            320,
            &[(100, 90), (220, 90), (20, 20), (300, 160)],
        )
    };
    let mid = [127, 64, 0];
    assert_colors(
        "wipe",
        &half(TransitionKind::WipeLeft),
        &[RED, GREEN, BLUE, BLUE],
    );
    assert_colors("mix", &half(TransitionKind::Mix), &[mid, mid, BLUE, BLUE]);
    assert_colors(
        "dip",
        &half(TransitionKind::DipBlack),
        &[BLACK, BLACK, BLACK, BLACK],
    );
}

/// Yellow, green, red and dark blue quadrants (top-left, top-right,
/// bottom-left, bottom-right), so a flip, turn or crop shows which corner
/// went where.
const TL: [u8; 3] = [255, 255, 64];
const TR: [u8; 3] = [0, 255, 64];
const BL: [u8; 3] = [255, 0, 64];
const BR: [u8; 3] = [0, 0, 64];
const BLACK: [u8; 3] = [0, 0, 0];

/// Export `quadrants.mp4` (320x180) under `transform` for 0.3 s and read
/// the middle frame's colour at each `(x, y)`.
fn placed(
    format: Format,
    transform: Transform,
    background: Background,
    points: &[(usize, usize)],
) -> ((u32, u32), Vec<[u8; 3]>) {
    let fixture = Fixture::new();
    let source = fixture.video(
        "quadrants.mp4",
        "nullsrc=s=320x180:r=10:d=1",
        "format=gbrp,geq=r='255*lt(X,W/2)':g='255*lt(Y,H/2)':b=64",
        None,
    );
    let mut timeline = Timeline::new(10.0).with_format(format);
    let id = video(&mut timeline, &source, 0, 0.0, 0.3, 0.0);
    let clip = timeline.clip_mut(id).unwrap();
    clip.transform = transform;
    clip.background = background;
    let exported = fixture.export("placed.mp4", &timeline);
    let info = probe(exported.to_str().unwrap()).unwrap();
    let frame = &frames(&exported)[1];
    let colors = points
        .iter()
        .map(|&(x, y)| {
            let at = (y * info.width as usize + x) * 3;
            [frame[at], frame[at + 1], frame[at + 2]]
        })
        .collect();
    ((info.width, info.height), colors)
}

fn assert_colors(label: &str, actual: &[[u8; 3]], expected: &[[u8; 3]]) {
    for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a.iter().zip(e).all(|(a, e)| a.abs_diff(*e) <= 24),
            "{label}: point {index} is {a:?}, expected {e:?} (all: {actual:?})"
        );
    }
}

#[test]
fn landscape_video_on_a_portrait_canvas_is_letterboxed() {
    // 320x180 fits 180 wide: 102 tall, centred at y 160.
    let (canvas, colors) = placed(
        Format::Portrait,
        Transform::default(),
        Background::Black,
        &[(45, 135), (135, 185), (90, 40), (90, 280)],
    );
    assert_eq!(canvas, (180, 320));
    assert_colors("letterbox", &colors, &[TL, BR, BLACK, BLACK]);
}

#[test]
fn scale_and_position_move_the_picture() {
    // Half size, centred a quarter right and a quarter up: x 160-320, y 0-90.
    let t = Transform {
        scale: 0.5,
        x: 0.25,
        y: -0.25,
        ..Transform::default()
    };
    let (canvas, colors) = placed(
        Format::Landscape,
        t,
        Background::Black,
        &[(200, 22), (280, 67), (80, 135), (200, 130), (80, 45)],
    );
    assert_eq!(canvas, (320, 180));
    assert_colors("scale", &colors, &[TL, BR, BLACK, BLACK, BLACK]);
}

#[test]
fn crop_keeps_its_part_and_fits_it() {
    // The right half (160x180) fits by height, centred: x 80-240.
    let t = Transform {
        crop_left: 0.5,
        ..Transform::default()
    };
    let (_, colors) = placed(
        Format::Landscape,
        t,
        Background::Black,
        &[(160, 45), (160, 135), (40, 90), (280, 90)],
    );
    assert_colors("crop", &colors, &[TR, BR, BLACK, BLACK]);
}

#[test]
fn flips_mirror_the_picture() {
    let points = [(80, 45), (240, 45), (80, 135), (240, 135)];
    let flip_h = Transform {
        flip_h: true,
        ..Transform::default()
    };
    let (_, colors) = placed(Format::Landscape, flip_h, Background::Black, &points);
    assert_colors("flip_h", &colors, &[TR, TL, BR, BL]);
    let both = Transform {
        flip_v: true,
        ..flip_h
    };
    let (_, colors) = placed(Format::Landscape, both, Background::Black, &points);
    assert_colors("flip_h + flip_v", &colors, &[BR, BL, TR, TL]);
}

#[test]
fn rotation_turns_the_picture_clockwise() {
    // A quarter turn clockwise: the top edge goes right, the left edge up.
    // The 180x320 box is centred, x 70-250.
    let t = Transform {
        rotation: 90.0,
        ..Transform::default()
    };
    let (_, colors) = placed(
        Format::Landscape,
        t,
        Background::Black,
        &[
            (205, 45),
            (115, 45),
            (205, 135),
            (115, 135),
            (30, 90),
            (290, 90),
        ],
    );
    assert_colors("rotate 90", &colors, &[TL, BL, TR, BR, BLACK, BLACK]);
}

#[test]
fn blurred_background_fills_the_bars_with_the_picture() {
    let (_, colors) = placed(
        Format::Portrait,
        Transform::default(),
        Background::Blur,
        &[(45, 135), (135, 185), (90, 30), (90, 290)],
    );
    assert_colors("picture", &colors[..2], &[TL, BR]);
    // Above: the picture's green top half, enlarged and blurred; below:
    // its bottom, which has no green.
    let [above, below] = [colors[2], colors[3]];
    assert!(above[1] > 150 && above[2] > 30, "above {above:?}");
    assert!(below[1] < 100 && below[2] > 30, "below {below:?}");
}

/// The quadrants as one 320x180 picture, written by FFmpeg as `name`.
/// A 2 s quadrants picture on lane 0 over blue on lane 1, keyed by `keys`
/// (property, clip time, value); the colours of frame `frame` (10 fps) at
/// each point.
fn keyed(
    keys: &[(Property, f64, f64)],
    sample: &[(usize, &[(usize, usize)])],
) -> Vec<Vec<[u8; 3]>> {
    eased(keys, openreel_engine::Ease::Linear, sample)
}

/// [`keyed`] with every key easing to the next along `ease`.
fn eased(
    keys: &[(Property, f64, f64)],
    ease: openreel_engine::Ease,
    sample: &[(usize, &[(usize, usize)])],
) -> Vec<Vec<[u8; 3]>> {
    let fixture = Fixture::new();
    let source = fixture.video(
        "quadrants.mp4",
        "nullsrc=s=320x180:r=10:d=3",
        "format=gbrp,geq=r='255*lt(X,W/2)':g='255*lt(Y,H/2)':b=64",
        None,
    );
    let blue = fixture.color("blue.mp4", "blue", None);
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &blue, 1, 0.0, 2.0, 0.0);
    let id = video(&mut timeline, &source, 0, 0.0, 2.0, 0.0);
    let clip = timeline.clip_mut(id).unwrap();
    for &(property, at, value) in keys {
        clip.keyframes.set(property, at, value);
        clip.keyframes.ease_key(property, at, ease);
    }
    let frames = frames(&fixture.export("keyed.mp4", &timeline));
    sample
        .iter()
        .map(|&(frame, points)| colors_at(&frames[frame], 320, points))
        .collect()
}

/// Scale from half to whole and position from a quarter left to a quarter
/// right over the first second, then held: each frame between keys sits
/// where the straight line between them puts it.
#[test]
fn keyed_scale_and_position_move_the_picture_between_keys() {
    let got = keyed(
        &[
            (Property::Scale, 0.0, 0.5),
            (Property::Scale, 1.0, 1.0),
            (Property::X, 0.0, -0.25),
            (Property::X, 1.0, 0.25),
        ],
        &[
            // 160x90 centred at x 80: x 0-160, y 45-135.
            (0, &[(40, 67), (120, 112), (240, 90)]),
            // 0.5 s: 240x135 centred: x 40-280, y 22-157.
            (5, &[(60, 40), (260, 140), (20, 90), (300, 90), (160, 10)]),
            // Held after the last key: 320x180 centred at x 240.
            (15, &[(100, 20), (300, 160), (40, 90)]),
        ],
    );
    assert_colors("start", &got[0], &[TL, BR, BLUE]);
    assert_colors("halfway", &got[1], &[TL, BR, BLUE, BLUE, BLUE]);
    assert_colors("held", &got[2], &[TL, BR, BLUE]);
}

/// Rotation from 0 to 90 degrees over the first second: 45 degrees
/// halfway turns the canvas corner off the picture, and the held quarter
/// turn puts the top-left quadrant top right.
#[test]
fn keyed_rotation_turns_the_picture_between_keys() {
    let got = keyed(
        &[
            (Property::Rotation, 0.0, 0.0),
            (Property::Rotation, 1.0, 90.0),
        ],
        &[
            (0, &[(5, 5), (315, 175)]),
            // (5, 5) is 155 left and 85 up of the centre: at 45 degrees
            // the picture's left edge passes 10 points inside it. The top
            // left quadrant's (-40, -20) turns to (-14, -42).
            (5, &[(5, 5), (146, 48)]),
            // 320x180 turned a quarter: x 70-250.
            (15, &[(200, 50), (120, 130), (30, 90)]),
        ],
    );
    assert_colors("upright", &got[0], &[TL, BR]);
    assert_colors("45 degrees", &got[1], &[BLUE, TL]);
    assert_colors("turned", &got[2], &[TL, BR, BLUE]);
}

/// Opacity from 0 to 1 over the first second: each frame mixes the picture
/// and the blue under it by the straight line's value at its time.
#[test]
fn keyed_opacity_fades_the_picture_between_keys() {
    let points: &[(usize, usize)] = &[(80, 45)];
    let got = keyed(
        &[(Property::Opacity, 0.0, 0.0), (Property::Opacity, 1.0, 1.0)],
        &[(2, points), (5, points), (8, points), (15, points)],
    );
    for (sample, a) in got.iter().zip([0.2_f64, 0.5, 0.8, 1.0]) {
        let want: Vec<u8> = TL
            .iter()
            .zip(BLUE)
            .map(|(&p, b)| (f64::from(p) * a + f64::from(b) * (1.0 - a)).round() as u8)
            .collect();
        let error = sample[0]
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(error <= 6, "opacity {a}: {:?}, want {want:?}", sample[0]);
    }
}

/// Opacity from 0 to 1 over the first second, easing in and out: t³ x 4
/// to halfway, then its mirror, so 0.032 at 0.2 s, 0.5 at 0.5 s and 0.968
/// at 0.8 s, where a straight line gives 0.2 and 0.8. Export fades by
/// `sendcmd`, whose commands cannot hold the `if` that joins the halves.
#[test]
fn eased_opacity_fades_along_its_curve() {
    let points: &[(usize, usize)] = &[(80, 45)];
    let got = eased(
        &[(Property::Opacity, 0.0, 0.0), (Property::Opacity, 1.0, 1.0)],
        openreel_engine::Ease::EaseInOut,
        &[(2, points), (5, points), (8, points)],
    );
    for (sample, a) in got.iter().zip([0.032_f64, 0.5, 0.968]) {
        let want: Vec<u8> = TL
            .iter()
            .zip(BLUE)
            .map(|(&p, b)| (f64::from(p) * a + f64::from(b) * (1.0 - a)).round() as u8)
            .collect();
        let error = sample[0]
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(error <= 6, "opacity {a}: {:?}, want {want:?}", sample[0]);
    }
}

/// Position from a quarter left to a quarter right over the first second,
/// easing in (t³). At 0.2 s the centre is 0.004 of the way, x -0.246: the
/// 320 px picture spans -79 to 241, so x 250 shows the blue under it (a
/// straight line puts it at -48 to 272). At 0.5 s it is 0.125 of the way,
/// x -0.1875: -60 to 260, so x 110 is 170 into the picture, its top right
/// quadrant (the straight line's top left), and x 270 is blue.
#[test]
fn eased_position_moves_along_its_curve() {
    let got = eased(
        &[(Property::X, 0.0, -0.25), (Property::X, 1.0, 0.25)],
        openreel_engine::Ease::EaseIn,
        &[
            (2, &[(250, 90), (230, 40)]),
            (5, &[(110, 40), (270, 130), (250, 130)]),
            (15, &[(100, 20), (40, 90)]),
        ],
    );
    assert_colors("0.2 s", &got[0], &[BLUE, TR]);
    assert_colors("0.5 s", &got[1], &[TR, BLUE, BR]);
    assert_colors("held", &got[2], &[TL, BLUE]);
}

fn quadrant_photo(fixture: &Fixture, name: &str) -> PathBuf {
    let path = fixture.0.join(name);
    let quadrants = "format=gbrp,geq=r='255*lt(X,W/2)':g='255*lt(Y,H/2)':b=64";
    // A GIF keeps the four colours exactly in a palette made for them,
    // which needs the end of the one-frame input.
    let filter = if name.ends_with(".gif") {
        format!("{quadrants},trim=end_frame=1,split[a][b];[a]palettegen[p];[b][p]paletteuse")
    } else {
        quadrants.to_owned()
    };
    run(Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("nullsrc=s=320x180")
        .args(["-vf", &filter])
        .args(["-frames:v", "1", "-q:v", "2"])
        .arg(&path));
    path
}

/// A photo clip at `start` for `duration`, as the editor adds one.
fn photo(timeline: &mut Timeline, path: &Path, start: f64, duration: f64) -> u64 {
    let id = video(timeline, path.to_str().unwrap(), 0, start, duration, 0.0);
    timeline.clip_mut(id).unwrap().photo = true;
    id
}

/// Read `(x, y)` from each of `frame`'s pixels in a `width`-wide RGB frame.
fn colors_at(frame: &[u8], width: u32, points: &[(usize, usize)]) -> Vec<[u8; 3]> {
    points
        .iter()
        .map(|&(x, y)| {
            let at = (y * width as usize + x) * 3;
            [frame[at], frame[at + 1], frame[at + 2]]
        })
        .collect()
}

/// A PNG, and a GIF of one frame, which lasts its 0.04 s delay as video.
#[test]
fn a_photo_is_held_for_its_whole_clip_and_letterboxed() {
    let fixture = Fixture::new();
    for name in ["quadrants.png", "quadrants.gif"] {
        let path = quadrant_photo(&fixture, name);
        let info = probe(path.to_str().unwrap()).unwrap();
        assert!(info.photo && info.has_video, "{name}: {info:?}");
        assert_eq!((info.width, info.height, info.duration), (320, 180, 0.0));
        // Black for 0.2 s, then the photo for 0.5 s: frames 2 to 6.
        let mut timeline = Timeline::new(10.0).with_format(Format::Portrait);
        photo(&mut timeline, &path, 0.2, 0.5);
        let exported = fixture.export(&format!("{name}.mp4"), &timeline);
        let frames = frames(&exported);
        assert_eq!(frames.len(), 7, "{name}: every frame of the timeline");
        let points = [(45, 135), (135, 185), (90, 40), (90, 280)];
        for (index, frame) in frames.iter().enumerate() {
            let expected = if index < 2 {
                [BLACK; 4]
            } else {
                [TL, BR, BLACK, BLACK]
            };
            assert_colors(
                &format!("{name} frame {index}"),
                &colors_at(frame, 180, &points),
                &expected,
            );
        }
    }
}

/// A GIF of more than one frame is video: it lasts its frames and plays
/// them, red for 0.2 s and then blue.
#[test]
fn an_animated_gif_plays_as_video() {
    let fixture = Fixture::new();
    let gif = fixture.0.join("two.gif");
    run(Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("nullsrc=s=64x32:r=5:d=0.4")
        .args([
            "-vf",
            "format=gbrp,geq=r='255*lt(N,1)':g=0:b='255*gte(N,1)',\
             split[a][b];[a]palettegen[p];[b][p]paletteuse",
        ])
        .arg(&gif));
    let info = probe(gif.to_str().unwrap()).unwrap();
    assert!(!info.photo && info.has_video, "{info:?}");
    assert_eq!((info.width, info.height, info.duration), (64, 32, 0.4));
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, gif.to_str().unwrap(), 0, 0.0, 0.4, 0.0);
    let frames = frames(&fixture.export("two.mp4", &timeline));
    assert_eq!(frames.len(), 4);
    for (index, frame) in frames.iter().enumerate() {
        let expected = if index < 2 { RED } else { BLUE };
        assert_colors(
            &format!("frame {index}"),
            &colors_at(frame, 64, &[(32, 16)]),
            &[expected],
        );
    }
}

/// `jpeg` with an EXIF block saying how to turn it for display.
#[test]
fn a_freeze_frame_holds_the_picture_its_point_shows() {
    let fixture = Fixture::new();
    // Each frame three levels brighter than the last, so a neighbour fails
    // the comparison.
    let source = fixture.video(
        "ramp.mp4",
        "nullsrc=s=64x36:r=30:d=2.5",
        "geq=lum='20+3*N':cb=128:cr=128",
        None,
    );
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(30.0);
    let id = video(&mut timeline, &source, 0, 0.0, 2.0, 0.0);
    // Past the first second, so export seeks before it takes the picture.
    // 0.4 of a frame past 1.5 s, frame 45 is on screen.
    timeline.freeze_frame(id, 1.5 + 0.4 / 30.0, 0.2).unwrap();
    let exported = frames(&fixture.export("freeze.mp4", &timeline));
    let mut expected: Vec<&[u8]> = original[..45].iter().map(Vec::as_slice).collect();
    expected.extend(std::iter::repeat_n(original[45].as_slice(), 6));
    expected.extend(original[45..60].iter().map(Vec::as_slice));
    assert_frames("frame 45 held for 6 frames", &exported, &expected);
}

fn with_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
    let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01".to_vec();
    // Tag 0x0112 (Orientation), SHORT, one value; then no next IFD.
    exif.extend([0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, orientation, 0, 0]);
    exif.extend([0, 0, 0, 0]);
    let length = (exif.len() + 2) as u16;
    let mut out = jpeg[..2].to_vec();
    out.extend([0xff, 0xe1]);
    out.extend(length.to_be_bytes());
    out.extend(exif);
    out.extend(&jpeg[2..]);
    out
}

/// A phone saves a photo as its sensor read it and says in EXIF how to show
/// it: turned (3, 6, 8), mirrored (2, 4) or both (5, 7). Each quadrant of
/// the stored picture shows where the EXIF standard puts it, in the
/// preview's decoder for all eight and in export for a turn and a
/// mirrored turn.
#[test]
fn phone_photos_show_turned_and_mirrored_as_their_exif_orientation_says() {
    let fixture = Fixture::new();
    let stored = std::fs::read(quadrant_photo(&fixture, "stored.jpg")).unwrap();
    // The stored quadrant shown top-left, top-right, bottom-left and
    // bottom-right.
    let shown = [
        (1, [TL, TR, BL, BR]),
        (2, [TR, TL, BR, BL]),
        (3, [BR, BL, TR, TL]),
        (4, [BL, BR, TL, TR]),
        (5, [TL, BL, TR, BR]),
        (6, [BL, TL, BR, TR]),
        (7, [BR, TR, BL, TL]),
        (8, [TR, BR, TL, BL]),
    ];
    for (orientation, expected) in shown {
        let path = fixture.0.join(format!("o{orientation}.jpg"));
        std::fs::write(&path, with_orientation(&stored, orientation)).unwrap();
        let path = path.to_str().unwrap();
        let size = if orientation >= 5 {
            (180, 320)
        } else {
            (320, 180)
        };
        let info = probe(path).unwrap();
        assert_eq!((info.width, info.height), size, "{orientation}: size");
        let picture = VideoDecoder::open(path, size.0, false)
            .unwrap()
            .frame_at(0.0)
            .unwrap();
        assert_eq!((picture.width, picture.height), size);
        let rgb: Vec<u8> = picture
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| &pixel[..3])
            .copied()
            .collect();
        let (w, h) = (size.0 as usize, size.1 as usize);
        let quadrants = [
            (w / 4, h / 4),
            (w * 3 / 4, h / 4),
            (w / 4, h * 3 / 4),
            (w * 3 / 4, h * 3 / 4),
        ];
        assert_colors(
            &format!("orientation {orientation} preview"),
            &colors_at(&rgb, size.0, &quadrants),
            &expected,
        );
        if ![6, 7].contains(&orientation) {
            continue;
        }
        let mut timeline = Timeline::new(10.0).with_format(Format::Landscape);
        photo(&mut timeline, Path::new(path), 0.0, 0.3);
        let exported = fixture.export(&format!("o{orientation}.mp4"), &timeline);
        let frames = frames(&exported);
        assert_eq!(frames.len(), 3);
        // 180x320 fits 101x180, centred: x 110-210.
        let points = [
            (135, 45),
            (185, 45),
            (135, 135),
            (185, 135),
            (30, 90),
            (290, 90),
        ];
        let mut expected = expected.to_vec();
        expected.extend([BLACK, BLACK]);
        for (index, frame) in frames.iter().enumerate() {
            assert_colors(
                &format!("orientation {orientation} export frame {index}"),
                &colors_at(frame, 320, &points),
                &expected,
            );
        }
    }
}

/// HEIC photos as libheif's `heif-enc` saves them (`tests/data/README.md`).
/// Each holds one 600x350 picture whose 128-pixel blocks each have their
/// own colour, so a tile in the wrong place shows. `grid.heic` is saved as
/// iPhones save photos: 128-pixel tiles (a 640x384 grid cropped back to
/// 600x350) turned a quarter clockwise by the grid's `irot`, so it shows
/// 350x600. `mirrored.heic` is a tile grid shown mirrored and turned
/// (EXIF orientation 7, as heif-enc stores it: `imir`). `single.heic` is
/// one image with a thumbnail beside it.
#[test]
fn heic_photos_preview_and_export_upright_with_every_tile_in_place() {
    let fixture = Fixture::new();
    // Where the displayed picture shows the stored picture's (x, y).
    type Place = fn(usize, usize) -> (usize, usize);
    let photos: [(&str, Place); 3] = [
        // A quarter turn clockwise: the stored bottom-left shows top-left.
        ("grid.heic", |x, y| (349 - y, x)),
        // Flipped about the other diagonal: bottom-right shows top-left.
        ("mirrored.heic", |x, y| (349 - y, 599 - x)),
        ("single.heic", |x, y| (x, y)),
    ];
    for (name, place) in photos {
        let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
        let info = probe(&path).unwrap();
        assert!(info.photo && info.has_video, "{name}: {info:?}");
        let size = if name == "single.heic" {
            (600, 350)
        } else {
            (350, 600)
        };
        assert_eq!(
            (info.width, info.height, info.duration),
            (size.0, size.1, 0.0)
        );
        // Each block's middle (the right and bottom blocks are cut short),
        // where the displayed picture shows it, and its colour.
        let mut points = Vec::new();
        let mut expected = Vec::new();
        for column in 0..5 {
            for row in 0..3 {
                let (x, y) = ((64 + 128 * column).min(556), (64 + 128 * row).min(303));
                points.push(place(x, y));
                expected.push([30 + 45 * column as u8, 40 + 80 * row as u8, 128]);
            }
        }
        // The preview's decoder joins the tiles itself...
        let picture = VideoDecoder::open(&path, size.0, false)
            .unwrap()
            .frame_at(0.0)
            .unwrap();
        assert_eq!(
            (picture.width, picture.height),
            size,
            "{name}: preview size"
        );
        let rgb: Vec<u8> = picture
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|pixel| &pixel[..3])
            .copied()
            .collect();
        assert_colors(
            &format!("{name} preview"),
            &colors_at(&rgb, size.0, &points),
            &expected,
        );
        // ...and export has the FFmpeg CLI join them.
        let mut timeline = Timeline::new(10.0);
        photo(&mut timeline, Path::new(&path), 0.0, 0.2);
        let exported = fixture.export(&format!("{name}.mp4"), &timeline);
        let out = probe(exported.to_str().unwrap()).unwrap();
        assert_eq!((out.width, out.height), size, "{name}: export size");
        let frames = frames(&exported);
        assert_eq!(frames.len(), 2, "{name}");
        for (index, frame) in frames.iter().enumerate() {
            assert_colors(
                &format!("{name} export frame {index}"),
                &colors_at(frame, size.0, &points),
                &expected,
            );
        }
    }
}

/// Eight flat patches, four across and two down, on a 320x180 source:
/// darks, greys, light, saturated primaries, skin and yellow.
const PATCHES: [[u8; 3]; 8] = [
    [30, 30, 30],
    [128, 128, 128],
    [225, 225, 225],
    [200, 60, 30],
    [60, 170, 80],
    [40, 70, 190],
    [230, 180, 150],
    [250, 240, 90],
];

/// `name`, 320x180 at 10 fps for 1 s: eight 80x90 patches, patch `i`
/// showing `PATCHES[(i + shift) % 8]`.
fn patch_source(fixture: &Fixture, name: &str, shift: usize) -> String {
    colour_patches(
        fixture,
        name,
        std::array::from_fn(|i| PATCHES[(i + shift) % 8]),
    )
}

/// `name`, 320x180 at 10 fps for 1 s: eight 80x90 patches, patch `i`
/// showing `colours[i]`.
fn colour_patches(fixture: &Fixture, name: &str, colours: [[u8; 3]; 8]) -> String {
    let pick = |c: usize| {
        let at = |i: usize| format!("if(eq(floor(X/80)+4*floor(Y/90),{i}),{},", colours[i][c]);
        let mut e: String = (0..8).map(at).collect();
        e.push('0');
        e.push_str(&")".repeat(8));
        e
    };
    let filter = format!(
        "format=gbrp,geq=r='{}':g='{}':b='{}'",
        pick(0),
        pick(1),
        pick(2)
    );
    fixture.video(name, "nullsrc=s=320x180:r=10:d=1", &filter, None)
}

/// The middle of patch `i` in a 320x180 RGB frame.
fn patch(frame: &[u8], i: usize) -> [u8; 3] {
    let (x, y) = (40 + 80 * (i % 4), 45 + 90 * (i / 4));
    let at = (y * 320 + x) * 3;
    [frame[at], frame[at + 1], frame[at + 2]]
}

/// Every blend mode mixes a picture with the lane under it as the preview
/// does. Patches lie over patches in another order, so each patch meets a
/// different colour; one mode runs at 60% opacity. Export carries each
/// picture through the canvas's YUV, so its answer may be that of inputs
/// two levels off.
#[test]
fn blend_modes_mix_a_picture_with_the_lane_under_it_as_the_preview_does() {
    use openreel_engine::BlendMode;
    let fixture = Fixture::new();
    let under = patch_source(&fixture, "under.mp4", 0);
    let over = patch_source(&fixture, "over.mp4", 3);
    let (under_rgb, over_rgb) = (
        frames(Path::new(&under))[1].clone(),
        frames(Path::new(&over))[1].clone(),
    );
    let modes = &BlendMode::ALL[1..];
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &under, 1, 0.0, 0.3 * modes.len() as f64, 0.0);
    let faded = 3;
    for (index, &mode) in modes.iter().enumerate() {
        let id = video(&mut timeline, &over, 0, index as f64 * 0.3, 0.3, 0.0);
        let clip = timeline.clip_mut(id).unwrap();
        clip.blend = mode;
        if index == faded {
            clip.opacity = 0.6;
        }
    }
    let exported = frames(&fixture.export("blend.mp4", &timeline));
    let mut worst = 0;
    for (index, &mode) in modes.iter().enumerate() {
        let alpha = if index == faded { 0.6 } else { 1.0 };
        let table = mode.table();
        let mix = |u: i32, o: i32| {
            let (u, o) = (u.clamp(0, 255) as usize, o.clamp(0, 255) as usize);
            let blended = f64::from(table[u * 256 + o]);
            (u as f64 + alpha * (blended - u as f64)).round() as i32
        };
        for i in 0..8 {
            let (u, o) = (patch(&under_rgb, i), patch(&over_rgb, i));
            let got = patch(&exported[index * 3 + 1], i);
            for c in 0..3 {
                let (u, o) = (i32::from(u[c]), i32::from(o[c]));
                let near: Vec<i32> = (-2..=2)
                    .flat_map(|du| (-2..=2).map(move |dov| (du, dov)))
                    .map(|(du, dov)| mix(u + du, o + dov))
                    .collect();
                let (low, high) = (*near.iter().min().unwrap(), *near.iter().max().unwrap());
                let got = i32::from(got[c]);
                let off = (low - got).max(got - high).max(0);
                worst = worst.max(off);
                // An encode moves a flat patch by up to 3 levels.
                assert!(
                    off <= 4,
                    "{mode:?}, patch {i}, channel {c}: under {u}, over {o}, export {got}, \
                     preview {} ({low} to {high} near it)",
                    mix(u, o)
                );
            }
        }
    }
    eprintln!("blend patches: worst {worst} levels past the preview's range");
}

/// A chroma key clears a green screen, lit evenly, in shadow or bright,
/// so the lane under it shows, and keeps skin, dark hair, white and grey;
/// a green cast on skin loses half its green over the mean of the other
/// two. Export's `chromakey` and `despill` agree with the preview's maths
/// on every patch, including one half keyed, for green, red and blue
/// keys.
#[test]
fn chroma_key_clears_the_screen_and_previews_as_it_exports() {
    use openreel_engine::{ChromaKey, Rgb, KEY_BLUE};
    let fixture = Fixture::new();
    let red = colour_patches(&fixture, "red.mp4", [[255, 0, 0]; 8]);
    let colours = [
        [0, 177, 64],    // the screen
        [20, 110, 45],   // the screen in shadow
        [70, 205, 100],  // the screen lit bright
        [220, 170, 140], // skin
        [40, 30, 25],    // dark hair
        [235, 235, 235], // white
        [160, 185, 140], // skin with green spill
        [110, 165, 115], // greyish green, half keyed
    ];
    let source = colour_patches(&fixture, "screen.mp4", colours);
    let decoded = &frames(Path::new(&source))[5];
    let keys = [
        ChromaKey::default(),
        ChromaKey {
            color: Rgb(200, 40, 60),
            spill: 1.0,
            ..ChromaKey::default()
        },
        ChromaKey {
            color: KEY_BLUE,
            strength: 0.8,
            softness: 0.0,
            spill: 1.0,
        },
    ];
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &red, 1, 0.0, 1.2, 0.0);
    for (index, key) in keys.iter().enumerate() {
        let id = video(&mut timeline, &source, 0, index as f64 * 0.4, 0.4, 0.0);
        timeline.clip_mut(id).unwrap().chroma = Some(*key);
    }
    let exported = frames(&fixture.export("keyed.mp4", &timeline));

    let mut worst = 0;
    for (index, key) in keys.iter().enumerate() {
        let mut rgba: Vec<u8> = decoded
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect();
        key.apply_rgba(&mut rgba, 320);
        let got = &exported[index * 4 + 2];
        for i in 0..8 {
            let (x, y) = (40 + 80 * (i % 4), 45 + 90 * (i / 4));
            let px = &rgba[(y * 320 + x) * 4..][..4];
            let alpha = f64::from(px[3]) / 255.0;
            let preview: [f64; 3] = std::array::from_fn(|c| {
                let under = if c == 0 { 255.0 } else { 0.0 };
                under + alpha * (f64::from(px[c]) - under)
            });
            let exported = patch(got, i);
            if index == 0 {
                // What the key should do, from its definition.
                match i {
                    0..=2 => assert_eq!(px[3], 0, "screen patch {i} kept: {px:?}"),
                    3..=6 => assert_eq!(px[3], 255, "patch {i} cleared: {px:?}"),
                    _ => assert!((40..216).contains(&px[3]), "half keyed: {px:?}"),
                }
                if i == 6 {
                    let [r, g, b] = decoded[(y * 320 + x) * 3..][..3] else {
                        unreachable!()
                    };
                    let pulled = f64::from(g) - 0.5 * (f64::from(g) - f64::from(r / 2 + b / 2));
                    assert!(
                        (f64::from(px[1]) - pulled).abs() <= 1.5,
                        "spill: green {g} became {}, not {pulled}",
                        px[1]
                    );
                }
            }
            for c in 0..3 {
                let off = (preview[c] - f64::from(exported[c])).abs().round() as i32;
                worst = worst.max(off);
                assert!(
                    off <= 5,
                    "key {index}, patch {i}, channel {c}: preview {:.0}, export {}",
                    preview[c],
                    exported[c]
                );
            }
        }
    }
    eprintln!("chroma key patches: worst {worst} levels from the preview");
}

/// Masks clear a picture outside their shape, laid on the picture as it
/// shows: after its crop and flip, turning and moving with it. Three
/// masked clips over blue export as the CPU compositor (contact sheets,
/// the blended monitor) lays them, and the mask's centre and the point
/// mirrored from it land where the mask says. A masked photo over its
/// blurred copy shows that copy where the mask clears it, not the blue.
#[test]
fn masks_clear_the_picture_outside_their_shape_as_the_preview_does() {
    use openreel_engine::{place, BlendMode, Mask, MaskShape, Placement};
    use openreel_media::{Canvas, Image, Layer};
    let fixture = Fixture::new();
    let blue = fixture.color("blue.mp4", "blue", None);
    let source = fixture.video(
        "quadrants.mp4",
        "nullsrc=s=320x180:r=10:d=1",
        "format=gbrp,geq=r='255*lt(X,W/2)':g='255*lt(Y,H/2)':b=64",
        None,
    );
    let decoded: Vec<u8> = frames(Path::new(&source))[1]
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|&[r, g, b]| [r, g, b, 255])
        .collect();
    let blue_frame: Vec<u8> = frames(Path::new(&blue))[1]
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|&[r, g, b]| [r, g, b, 255])
        .collect();
    let cases = [
        (
            Transform {
                scale: 0.8,
                x: 0.1,
                rotation: 25.0,
                flip_h: true,
                ..Transform::default()
            },
            Mask {
                x: 0.15,
                y: -0.1,
                width: 0.5,
                height: 0.7,
                ..Mask::new(MaskShape::Heart)
            },
        ),
        (
            Transform {
                rotation: 30.0,
                scale: 0.9,
                ..Transform::default()
            },
            Mask {
                rotation: 40.0,
                feather: 0.3,
                invert: true,
                ..Mask::new(MaskShape::Star)
            },
        ),
        (
            Transform {
                crop_left: 0.3,
                flip_v: true,
                ..Transform::default()
            },
            Mask {
                x: -0.2,
                y: 0.15,
                width: 0.8,
                height: 0.4,
                round: 0.6,
                ..Mask::new(MaskShape::Rectangle)
            },
        ),
    ];
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &blue, 1, 0.0, 1.2, 0.0);
    for (index, (transform, mask)) in cases.iter().enumerate() {
        let id = video(&mut timeline, &source, 0, index as f64 * 0.3, 0.3, 0.0);
        let clip = timeline.clip_mut(id).unwrap();
        clip.transform = *transform;
        clip.mask = Some(*mask);
    }
    // Half size and a quarter right, so the picture's quadrants meet at
    // x 240 and its blurred copy's at x 160: they differ at x 200.
    let still = photo(
        &mut timeline,
        &quadrant_photo(&fixture, "quadrants.jpg"),
        0.9,
        0.3,
    );
    let clip = timeline.clip_mut(still).unwrap();
    clip.transform = Transform {
        scale: 0.5,
        x: 0.25,
        ..Transform::default()
    };
    clip.background = Background::Blur;
    // A circle about canvas (200, 72): the picture's top-left quadrant.
    clip.mask = Some(Mask {
        x: -0.25,
        y: -0.2,
        width: 0.3,
        height: 0.3,
        ..Mask::new(MaskShape::Circle)
    });
    let exported = frames(&fixture.export("masked.mp4", &timeline));
    let [kept, cleared, corner] =
        colors_at(&exported[10], 320, &[(200, 72), (200, 120), (10, 170)])[..]
    else {
        unreachable!()
    };
    // Inside the circle the picture's yellow; outside it, where the
    // picture is red, its blurred copy's dark bottom-right; no blue.
    assert!(
        kept[0] > 200 && kept[1] > 200,
        "inside the circle: {kept:?}"
    );
    assert!(
        cleared[0] < 60 && cleared[2] < 160,
        "outside the circle: {cleared:?}"
    );
    assert!(
        corner[2] < 160,
        "blue shows, so the blurred copy was cleared: {corner:?}"
    );
    let size = (320.0, 180.0);
    fn layer(rgba: &[u8], placement: Placement, mask: Option<Mask>) -> Layer<'_> {
        let size = (320.0, 180.0);
        let image = Image {
            width: 320,
            height: 180,
            rgba,
        };
        Layer {
            image,
            source: size,
            placement,
            opacity: 1.0,
            blur: false,
            blend: BlendMode::Normal,
            mask,
            shift: [0.0, 0.0],
            window: None,
        }
    }
    for (index, (transform, mask)) in cases.iter().enumerate() {
        let got = &exported[index * 3 + 1];
        let p = place(size, size, transform);
        let lay = |mask: Option<Mask>| {
            let mut canvas = Canvas::new(320, 180, size);
            canvas.draw(&layer(
                &blue_frame,
                place(size, size, &Transform::default()),
                None,
            ));
            canvas.draw(&layer(&decoded, p, mask));
            canvas.rgba
        };
        let (masked, plain) = (lay(Some(*mask)), lay(None));
        // Export's rotate and bicubic scaling draw hard edges (the
        // picture's own, its quadrants' borders and a hard mask's) a pixel
        // softer than the compositor's exact sampling, so pixels within two
        // of a hard edge in either frame are left out. A misplaced, mirrored
        // or wrongly feathered mask still differs over whole areas.
        let rgb = |frame: &[u8], x: usize, y: usize| -> [u8; 3] {
            let at = (y * 320 + x) * 4;
            [frame[at], frame[at + 1], frame[at + 2]]
        };
        let near_edge = |frame: &[u8], x: usize, y: usize| {
            let here = rgb(frame, x, y);
            (y.saturating_sub(2)..(y + 3).min(180)).any(|ny| {
                (x.saturating_sub(2)..(x + 3).min(320)).any(|nx| {
                    let there = rgb(frame, nx, ny);
                    (0..3).any(|c| here[c].abs_diff(there[c]) > 24)
                })
            })
        };
        let (mut total, mut counted, mut far) = (0.0, 0, 0);
        for y in 0..180 {
            for x in 0..320 {
                if near_edge(&plain, x, y) || near_edge(&masked, x, y) {
                    continue;
                }
                let preview = rgb(&masked, x, y);
                let export = &got[(y * 320 + x) * 3..][..3];
                let off = (0..3)
                    .map(|c| preview[c].abs_diff(export[c]))
                    .max()
                    .unwrap();
                total += f64::from(off);
                counted += 1;
                far += usize::from(off > 24);
            }
        }
        let mean = total / counted as f64;
        eprintln!(
            "mask {index}: mean {mean:.2} levels from the compositor over {counted} pixels, \
             {far} past 24"
        );
        assert!(
            counted > 320 * 180 / 2,
            "mask {index}: only {counted} pixels compared"
        );
        assert!(mean < 1.5, "mask {index}: mean error {mean:.2}");
        assert!(
            far < counted / 500,
            "mask {index}: {far} pixels differ by more than 24"
        );

        // From the definition: the canvas point of a share of the picture
        // as it shows, turned clockwise about its centre.
        let on_canvas = |(u, v): (f64, f64)| {
            let (dx, dy) = ((u - 0.5) * p.size[0], (v - 0.5) * p.size[1]);
            let (s, c) = p.rotation.to_radians().sin_cos();
            let (x, y) = (p.center[0] + dx * c - dy * s, p.center[1] + dx * s + dy * c);
            (y.round() as usize * 320 + x.round() as usize) * 3
        };
        let picture = |at: usize| got[at + 2] < 160;
        let centre = on_canvas((0.5 + mask.x, 0.5 + mask.y));
        let mirrored = on_canvas((0.5 - mask.x, 0.5 - mask.y));
        if index == 1 {
            // Inverted: the star's middle is cleared, its outside kept.
            assert!(!picture(centre), "star {index}: middle kept");
            assert!(
                picture(on_canvas((0.04, 0.5))),
                "star {index}: outside cleared"
            );
        } else {
            assert!(picture(centre), "mask {index}: its centre cleared");
            assert!(!picture(mirrored), "mask {index}: the mirrored point kept");
        }
    }
}

#[test]
fn shapes_and_stickers_export_over_the_lanes_under_them_as_the_preview_draws_them() {
    use openreel_engine::{place, BlendMode, Graphic, Rgb, Shape, ShapeKind};
    use openreel_media::{draw_graphic, write_graphic, Canvas, Image, Layer};
    let fixture = Fixture::new();
    let blue = fixture.color("blue.mp4", "blue", None);
    let blue_frame: Vec<u8> = frames(Path::new(&blue))[1]
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|&[r, g, b]| [r, g, b, 255])
        .collect();
    let circle = Graphic::Shape(Shape {
        fill: Some(Rgb(255, 214, 0)),
        outline: Some(Rgb(20, 20, 20)),
        thickness: 0.08,
        ..Shape::new(ShapeKind::Circle)
    });
    let sticker = Graphic::Sticker {
        name: "thumbs-up".to_owned(),
    };
    let cases = [
        (
            circle,
            Transform {
                scale: 0.5,
                x: -0.2,
                ..Transform::default()
            },
        ),
        (
            sticker,
            Transform {
                scale: 0.6,
                x: 0.15,
                rotation: 20.0,
                ..Transform::default()
            },
        ),
    ];
    // Each graphic starts on lane 0 at the same time as the video under
    // it, which would win the canvas's shape if graphics counted: the
    // canvas stays the video's 320x180, not a 2160-pixel square.
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &blue, 1, 0.0, 1.0, 0.0);
    let mut sources = Vec::new();
    for (index, (graphic, transform)) in cases.iter().enumerate() {
        let file = write_graphic(graphic, &fixture.0).unwrap();
        let id = photo(&mut timeline, &file.path, index as f64 * 0.5, 0.5);
        let clip = timeline.clip_mut(id).unwrap();
        clip.graphic = Some(graphic.clone());
        clip.transform = *transform;
        sources.push((f64::from(file.info.width), f64::from(file.info.height)));
    }
    let exported = fixture.export("graphics.mp4", &timeline);
    let info = probe(exported.to_str().unwrap()).unwrap();
    assert_eq!((info.width, info.height), (320, 180));
    let exported = frames(&exported);
    let size = (320.0, 180.0);
    for (index, (graphic, transform)) in cases.iter().enumerate() {
        let got = &exported[index * 5 + 2];
        let drawing = draw_graphic(graphic, 540).unwrap();
        let mut canvas = Canvas::new(320, 180, size);
        let under = |rgba| Layer {
            image: Image {
                width: 320,
                height: 180,
                rgba,
            },
            source: size,
            placement: place(size, size, &Transform::default()),
            opacity: 1.0,
            blur: false,
            blend: BlendMode::Normal,
            mask: None,
            shift: [0.0, 0.0],
            window: None,
        };
        canvas.draw(&under(&blue_frame));
        canvas.draw(&Layer {
            image: Image::from(&drawing),
            source: sources[index],
            placement: place(size, sources[index], transform),
            ..under(&blue_frame)
        });
        let preview = canvas.rgba;
        let rgb = |frame: &[u8], x: usize, y: usize| -> [u8; 3] {
            let at = (y * 320 + x) * 4;
            [frame[at], frame[at + 1], frame[at + 2]]
        };
        // Edges are drawn a pixel softer by export's scaling than by the
        // compositor's sampling, so pixels within two of one are left out.
        // A graphic drawn without its transparency covers its whole box,
        // and a misplaced one differs over whole areas.
        let near_edge = |x: usize, y: usize| {
            let here = rgb(&preview, x, y);
            (y.saturating_sub(2)..(y + 3).min(180)).any(|ny| {
                (x.saturating_sub(2)..(x + 3).min(320)).any(|nx| {
                    let there = rgb(&preview, nx, ny);
                    (0..3).any(|c| here[c].abs_diff(there[c]) > 24)
                })
            })
        };
        let (mut total, mut counted, mut drawn) = (0.0, 0, 0);
        for y in 0..180 {
            for x in 0..320 {
                if near_edge(x, y) {
                    continue;
                }
                let want = rgb(&preview, x, y);
                let have = &got[(y * 320 + x) * 3..][..3];
                total += f64::from((0..3).map(|c| want[c].abs_diff(have[c])).max().unwrap());
                counted += 1;
                drawn += usize::from(want[2] < 128);
            }
        }
        let mean = total / counted as f64;
        eprintln!("graphic {index}: mean {mean:.2} levels over {counted} pixels, {drawn} drawn");
        assert!(drawn > 1500, "graphic {index}: only {drawn} pixels drawn");
        assert!(mean < 2.0, "graphic {index}: mean error {mean:.2}");
        // The corner of the circle's box is clear: the blue shows there.
        if index == 0 {
            let p = place(size, sources[0], transform);
            let corner = (
                (p.center[0] - p.size[0] * 0.45) as usize,
                (p.center[1] - p.size[1] * 0.45) as usize,
            );
            let at = (corner.1 * 320 + corner.0) * 3;
            assert!(
                got[at + 2] > 200 && got[at] < 40,
                "box corner: {:?}",
                &got[at..at + 3]
            );
        }
    }
}

#[test]
fn graded_clips_export_as_the_preview_grades_them() {
    use openreel_engine::{ColorAdjust, Filter};
    let fixture = Fixture::new();
    let source = patch_source(&fixture, "patches.mp4", 0);
    let decoded = &frames(Path::new(&source))[1];
    let centre = |frame: &[u8], i: usize| {
        let (x, y) = (40 + 80 * (i % 4), 45 + 90 * (i / 4));
        let at = (y * 320 + x) * 3;
        [frame[at], frame[at + 1], frame[at + 2]]
    };
    let none = ColorAdjust::default();
    let grades = [
        // A matrix past colorchannelmixer's ±2, run halved and doubled.
        ColorAdjust {
            exposure: 2.0,
            contrast: 1.0,
            saturation: 1.0,
            ..none
        },
        ColorAdjust {
            filter: Filter::Sepia,
            strength: 0.6,
            temperature: -0.7,
            tint: 0.4,
            ..none
        },
        ColorAdjust {
            brightness: 0.6,
            filter: Filter::Vivid,
            ..none
        },
        ColorAdjust {
            brightness: -0.8,
            exposure: -1.5,
            saturation: -0.5,
            ..none
        },
        ColorAdjust {
            filter: Filter::Noir,
            ..none
        },
        ColorAdjust {
            filter: Filter::Fade,
            contrast: -1.0,
            ..none
        },
    ];
    let mut timeline = Timeline::new(10.0);
    for (index, grade) in grades.iter().enumerate() {
        let id = video(&mut timeline, &source, 0, index as f64 * 0.3, 0.3, 0.0);
        timeline.clip_mut(id).unwrap().color = *grade;
    }
    let exported = frames(&fixture.export("graded.mp4", &timeline));
    let mut worst = 0;
    for (index, grade) in grades.iter().enumerate() {
        // Decoding and encoding differ by a level or two; a strong grade
        // multiplies that by its largest row gain (about 11 for the first).
        let gain = grade
            .matrix()
            .iter()
            .map(|row| row[..3].iter().map(|v| v.abs()).sum::<f64>())
            .fold(0.0, f64::max);
        let limit = (3.0 + 0.5 * gain).ceil() as u8;
        for patch in 0..PATCHES.len() {
            let [r, g, b] = centre(decoded, patch);
            let mut preview = [r, g, b, 255];
            grade.apply_rgba(&mut preview);
            let got = centre(&exported[index * 3 + 1], patch);
            let error = (0..3).map(|c| got[c].abs_diff(preview[c])).max().unwrap();
            worst = worst.max(error);
            assert!(
                error <= limit,
                "grade {index} {grade:?}, patch {patch}: export {got:?}, preview {:?}",
                &preview[..3]
            );
        }
    }
    eprintln!("graded patches: worst channel difference {worst}");

    // The blurred background is graded with the picture: mono bars are grey.
    let mut timeline = Timeline::new(10.0).with_format(Format::Portrait);
    let id = video(&mut timeline, &source, 0, 0.0, 0.3, 0.0);
    let clip = timeline.clip_mut(id).unwrap();
    clip.background = Background::Blur;
    clip.color = ColorAdjust {
        filter: Filter::Mono,
        ..none
    };
    let frame = &frames(&fixture.export("graded-blur.mp4", &timeline))[1];
    for y in [20, 300] {
        let at = (y * 180 + 90) * 3;
        let [r, g, b] = [frame[at], frame[at + 1], frame[at + 2]];
        assert!(
            r.abs_diff(g) <= 3 && g.abs_diff(b) <= 3 && r > 40,
            "bar at y {y}: {:?}",
            [r, g, b]
        );
    }
}

/// Colour keys grade every exported frame with that frame's values: each
/// matches a plain grade of the values at its time, which
/// `graded_clips_export_as_the_preview_grades_them` holds to the preview.
/// The first clip's grades all fit the 8-bit mixer; the second's pass its
/// ±2 on every frame, so its matrix runs halved on float. Neighbouring
/// frames differ by 3 levels or more somewhere, so a grade a frame early or
/// late fails. A still holds its picture but not its grade: a photo goes
/// grey over its first second, its blurred background with it.
#[test]
fn keyed_colour_grades_each_frame_as_the_preview_does() {
    let fixture = Fixture::new();
    let source = patch_source(&fixture, "patches.mp4", 0);
    let mut timeline = Timeline::new(10.0);
    let fits = video(&mut timeline, &source, 0, 0.0, 1.0, 0.0);
    let float = video(&mut timeline, &source, 0, 1.0, 1.0, 0.0);
    for (id, keys) in [
        (
            fits,
            [
                (Property::Brightness, 0.0, -0.5),
                (Property::Brightness, 1.0, 0.5),
                (Property::Saturation, 0.2, 0.0),
                (Property::Saturation, 0.8, 0.6),
            ],
        ),
        (
            float,
            [
                (Property::Exposure, 0.2, 1.2),
                (Property::Exposure, 0.8, 2.0),
                (Property::Brightness, 0.0, -0.3),
                (Property::Brightness, 1.0, 0.3),
            ],
        ),
    ] {
        let clip = timeline.clip_mut(id).unwrap();
        clip.color.contrast = if id == float { 0.8 } else { 0.0 };
        for (property, at, value) in keys {
            clip.keyframes.set(property, at, value);
        }
    }
    let keyed = frames(&fixture.export("keyed-colour.mp4", &timeline));
    // The same frames, each a clip of its own with that frame's grade.
    let mut plain = Timeline::new(10.0);
    for n in 0..20 {
        let at = n as f64 / 10.0;
        let clip = timeline.clip(if n < 10 { fits } else { float }).unwrap();
        let id = video(&mut plain, &source, 0, at, 0.1, 0.0);
        plain.clip_mut(id).unwrap().color = clip.color_at(at);
    }
    let plain = frames(&fixture.export("plain-colour.mp4", &plain));
    assert_eq!((keyed.len(), plain.len()), (20, 20));
    let worst = |a: &[u8], b: &[u8]| {
        (0..PATCHES.len())
            .flat_map(|i| {
                let (a, b) = (patch(a, i), patch(b, i));
                (0..3).map(move |c| a[c].abs_diff(b[c]))
            })
            .max()
            .unwrap()
    };
    for n in 0..20 {
        let error = worst(&keyed[n], &plain[n]);
        assert!(
            error <= 1,
            "frame {n}: keyed and plain differ by {error} levels"
        );
    }

    let path = quadrant_photo(&fixture, "quadrants.png");
    let mut timeline = Timeline::new(10.0).with_format(Format::Portrait);
    let id = photo(&mut timeline, &path, 0.0, 1.5);
    let clip = timeline.clip_mut(id).unwrap();
    clip.background = Background::Blur;
    clip.keyframes.set(Property::Saturation, 0.0, 0.0);
    clip.keyframes.set(Property::Saturation, 1.0, -1.0);
    let clip = clip.clone();
    let exported = frames(&fixture.export("keyed-photo.mp4", &timeline));
    for n in [0, 5, 12] {
        let mut preview = [TL[0], TL[1], TL[2], 255];
        clip.color_at(n as f64 / 10.0).apply_rgba(&mut preview);
        let [got] = colors_at(&exported[n], 180, &[(45, 135)])[..] else {
            unreachable!()
        };
        let error = (0..3).map(|c| got[c].abs_diff(preview[c])).max().unwrap();
        assert!(
            error <= 6,
            "photo frame {n}: {got:?}, preview {:?}",
            &preview[..3]
        );
        let [bar] = colors_at(&exported[n], 180, &[(90, 20)])[..] else {
            unreachable!()
        };
        let spread = bar.iter().max().unwrap() - bar.iter().min().unwrap();
        if n == 12 {
            assert!(spread <= 4, "grey background at frame {n}: {bar:?}");
        } else if n == 0 {
            assert!(spread > 60, "coloured background at frame {n}: {bar:?}");
        }
    }
}

#[test]
fn occluded_video_resumes_at_advanced_source_time() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices.mp4", "10");
    let original = frames(Path::new(&source));
    let black = vec![0; original[0].len()];
    let cover = fixture.video("black.mp4", "color=c=black:s=64x36:r=10:d=1", "null", None);
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 1, 0.0, 0.6, 0.0);
    video(&mut timeline, &cover, 0, 0.2, 0.2, 0.0);
    let exported = fixture.export("occlusion.mp4", &timeline);
    assert_frames(
        "covered, then resumed",
        &frames(&exported),
        &[
            &original[0],
            &original[1],
            &black,
            &black,
            &original[4],
            &original[5],
        ],
    );
}

#[test]
fn titles_have_first_inserted_precedence_and_exclusive_ends() {
    let fixture = Fixture::new();
    let source = fixture.color("white.mp4", "white", None);
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 0.5, 0.0);
    // Punctuation the filter parsers treat specially is drawn as typed.
    let a = timeline.add_clip(
        r"FIRST: 100% 'WIDE' \TITLE",
        ClipKind::Title,
        9,
        0.0,
        0.2,
        0.0,
    );
    let b = timeline.add_clip("B", ClipKind::Title, 0, 0.1, 0.3, 0.0);
    let c = timeline.add_clip("C", ClipKind::Title, 0, 0.4, 0.1, 0.0);
    for id in [a, b, c] {
        timeline.clip_mut(id).unwrap().title_style = TitleStyle::Caption;
    }
    let overlap = fixture.export("titles-overlap.mp4", &timeline);
    let mut reference = timeline.clone();
    let b = reference.clip_mut(b).unwrap();
    b.start = 0.2;
    b.duration = 0.2;
    // Export each title alone to form an independent boundary oracle. The
    // reference graph must not accidentally validate an inclusive-end bug.
    let mut expected = Vec::new();
    for (id, count) in [(a, 2), (b.id, 2), (c, 1)] {
        let mut single = reference.clone();
        single
            .clips
            .retain(|clip| clip.kind != ClipKind::Title || clip.id == id);
        let title = single.clip_mut(id).unwrap();
        title.start = 0.0;
        title.duration = 0.5;
        let out = fixture.export(&format!("title-{id}.mp4"), &single);
        let decoded = frames(&out);
        let dark = decoded[0]
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|rgb| rgb[0] < 200)
            .count();
        assert!(
            dark > 100,
            "title {id} must actually be rendered, dark pixels={dark}"
        );
        expected.extend(std::iter::repeat_n(decoded[0].clone(), count));
    }
    let references: Vec<_> = expected.iter().map(Vec::as_slice).collect();
    assert_frames(
        "title precedence and half-open cuts",
        &frames(&overlap),
        &references,
    );
}

/// Yellow highlight pixels in an RGB frame: their count and mean x.
fn highlight_pixels(frame: &[u8], width: usize) -> (usize, f64) {
    let xs: Vec<usize> = frame
        .as_chunks::<3>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, rgb)| rgb[0] > 200 && rgb[1] > 160 && rgb[2] < 90)
        .map(|(index, _)| index % width)
        .collect();
    let mean = xs.iter().sum::<usize>() as f64 / xs.len().max(1) as f64;
    (xs.len(), mean)
}

#[test]
fn highlight_captions_light_only_the_word_being_spoken() {
    let fixture = Fixture::new();
    let source = fixture.video(
        "black.mp4",
        "color=c=black:s=1280x720:r=10:d=1",
        "null",
        None,
    );
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 1.0, 0.0);
    let id = timeline.add_clip("LEFTWORD RIGHTWORD", ClipKind::Title, 1, 0.0, 1.0, 0.0);
    let title = timeline.clip_mut(id).unwrap();
    title.title_style = TitleStyle::HighlightCaption;
    title.words = vec![0.0, 0.5];
    let decoded = frames(&fixture.export("highlight.mp4", &timeline));
    assert_eq!(decoded.len(), 10);
    let lit: Vec<_> = decoded.iter().map(|f| highlight_pixels(f, 1280)).collect();
    eprintln!("highlight pixels and mean x per frame: {lit:?}");
    assert!(
        lit.iter().all(|&(count, _)| count > 200),
        "a word is lit in every frame"
    );
    // The first word sits left of centre, the second right of it.
    assert!(
        lit[..5].iter().all(|&(_, x)| x < 600.0),
        "first word lit first"
    );
    assert!(
        lit[5..].iter().all(|&(_, x)| x > 680.0),
        "second word lit second"
    );

    // A renamed caption no longer matches its word times and lights nothing.
    timeline.clip_mut(id).unwrap().name = "ONE TWO THREE".to_owned();
    let renamed = frames(&fixture.export("renamed.mp4", &timeline));
    assert!(renamed.iter().all(|f| highlight_pixels(f, 1280).0 == 0));

    // Each word's layers quote their text like the line itself.
    timeline.clip_mut(id).unwrap().name = "IT'S: 100%".to_owned();
    let quoted = frames(&fixture.export("quoted.mp4", &timeline));
    assert!(
        quoted.iter().all(|f| highlight_pixels(f, 1280).0 > 100),
        "a word with punctuation is lit in every frame"
    );
}

/// The box (left, top, right, bottom) of the pixels `matches` picks, and
/// how many there are.
fn pixel_box(
    frame: &[u8],
    width: usize,
    matches: impl Fn(&[u8; 3]) -> bool,
) -> ([usize; 4], usize) {
    let (mut found, mut count) = ([usize::MAX, usize::MAX, 0, 0], 0);
    for (index, rgb) in frame.as_chunks::<3>().0.iter().enumerate() {
        if matches(rgb) {
            let (x, y) = (index % width, index / width);
            found = [
                found[0].min(x),
                found[1].min(y),
                found[2].max(x + 1),
                found[3].max(y + 1),
            ];
            count += 1;
        }
    }
    (found, count)
}

/// A caption of two lines lines them up left, centred or right, its last
/// line where a one-line caption's is and the line above one Inter line
/// higher. H's ink ends on the baseline: the last at 720 - 90 + ceil(0.96875
/// x 38) = 667, the first round(1.20996 x 38) = 46 px above.
#[test]
fn a_title_of_several_lines_exports_them_lined_up_and_growing_upward() {
    use openreel_engine::{TextAlign, TextLook};
    let fixture = Fixture::new();
    let source = fixture.video(
        "black.mp4",
        "color=c=black:s=1280x720:r=10:d=3",
        "null",
        None,
    );
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 3.0, 0.0);
    for (index, align) in TextAlign::ALL.into_iter().enumerate() {
        let id = timeline.add_clip(
            "HHHHHH\n\n  HH ",
            ClipKind::Title,
            0,
            index as f64,
            1.0,
            0.0,
        );
        let title = timeline.clip_mut(id).unwrap();
        title.title_style = TitleStyle::Caption;
        title.text = TextLook {
            align: Some(align),
            box_opacity: Some(0.0),
            ..TextLook::default()
        };
    }
    let decoded = frames(&fixture.export("lines.mp4", &timeline));
    for (index, align) in TextAlign::ALL.into_iter().enumerate() {
        let frame = &decoded[index * 10 + 5];
        let white = |p: &[u8; 3]| p.iter().all(|&v| v > 128);
        // The two lines: rows above and below the gap between them.
        let (whole, _) = pixel_box(frame, 1280, white);
        let rows = |from: usize, to: usize| {
            let part = &frame[from * 1280 * 3..to * 1280 * 3];
            let (b, n) = pixel_box(part, 1280, white);
            ([b[0], b[1] + from, b[2], b[3] + from], n)
        };
        let middle = (whole[1] + whole[3]) / 2;
        let ((first, a), (last, b)) = (rows(0, middle), rows(middle, 720));
        eprintln!("{align:?}: first {first:?} ({a} px), last {last:?} ({b} px)");
        assert!(a > 500 && b > 150, "two lines drawn: {a} and {b} px");
        assert!(
            (last[3] as i64 - 667).abs() <= 1 && (first[3] as i64 - 621).abs() <= 1,
            "{align:?}: baselines {} and {}",
            first[3],
            last[3]
        );
        // H advances 0.7432 em and its ink stands 0.0879 em in from each
        // end of the line: six are 162.8 px of ink at 38 px, two 49.8.
        let (first_w, last_w) = (first[2] - first[0], last[2] - last[0]);
        assert!(
            (first_w as f64 - 162.8).abs() <= 2.0 && (last_w as f64 - 49.8).abs() <= 2.0,
            "six H and two: {first_w} and {last_w} px"
        );
        let near = |a: f64, b: f64, by: f64| (a - b).abs() <= by;
        let mid = |b: [usize; 4]| (b[0] + b[2]) as f64 / 2.0;
        // An H stands 0.09 em (3.4 px) in from its advance on each side.
        let lined_up = match align {
            TextAlign::Left => {
                near(first[0] as f64, last[0] as f64, 1.0) && near(first[0] as f64, 45.4, 2.0)
            }
            TextAlign::Center => near(mid(first), mid(last), 1.5) && near(mid(first), 640.0, 2.0),
            TextAlign::Right => {
                near(first[2] as f64, last[2] as f64, 1.0) && near(first[2] as f64, 1234.6, 2.0)
            }
        };
        assert!(lined_up, "{align:?}: first {first:?}, last {last:?}");
    }
}

/// A Lora Bold Italic lower third exports in that face, where its font's
/// own metrics put it. The oracle is drawtext given each candidate file by
/// hand; the place comes from the font files' tables (read with fontTools).
#[test]
fn a_title_exports_in_its_own_font_weight_and_slant() {
    use openreel_engine::{TextLook, TitleFont};
    let fixture = Fixture::new();
    let source = fixture.video(
        "black.mp4",
        "color=c=black:s=1280x720:r=10:d=1",
        "null",
        None,
    );
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 1.0, 0.0);
    let id = timeline.add_clip("Kaleidoscope", ClipKind::Title, 0, 0.0, 1.0, 0.0);
    timeline.clip_mut(id).unwrap().text = TextLook {
        font: Some(TitleFont::Lora),
        bold: true,
        italic: true,
        box_opacity: Some(0.0),
        ..TextLook::default()
    };
    let decoded = frames(&fixture.export("font.mp4", &timeline));
    let lit = |frame: &[u8]| -> Vec<bool> {
        frame
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| p[0] > 128)
            .collect()
    };
    let exported = lit(&decoded[5]);

    // A lower third's line box keeps its lower edge: Inter's 44 px line is
    // 44 x 1.20996 = 53.24 px, so the edge is 110 - 53.24 = 56.76 px up.
    // Lora's line is 44 x 1.28 = 56.32, so its top is 113 px up, at 607.
    // FreeType's ascender, ceil(44 x 1.006) = 45, puts the baseline at
    // 652; the word's glyphs run from 0.763 em above it to 0.255 below.
    let (ink, count) = pixel_box(&decoded[5], 1280, |p| p[0] > 128);
    assert!(count > 1000, "the title is drawn: {count} px");
    let (top, bottom) = (652.0 - 0.763 * 44.0, 652.0 - -0.255 * 44.0);
    assert!(
        (ink[1] as f64 - top).abs() <= 2.0 && (ink[3] as f64 - bottom).abs() <= 2.0,
        "ink {ink:?}, rows {top:.1} to {bottom:.1} expected"
    );

    // The same word drawn from each candidate file: the export matches
    // Lora Bold Italic and no other face.
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fonts");
    let differs = |file: &str| -> usize {
        let out = run(Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg("color=c=black:s=1280x720:r=1:d=1")
            .arg("-vf")
            .arg(format!(
                "drawtext=fontfile={}:text=Kaleidoscope:fontsize=44:fontcolor=white:\
                 x=42:y=607:y_align=font",
                fonts.join(file).display()
            ))
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]));
        let drawn = lit(&out.stdout);
        drawn.iter().zip(&exported).filter(|(a, b)| a != b).count()
    };
    let right = differs("Lora-BoldItalic.ttf");
    for wrong in [
        "Lora-Regular.ttf",
        "Lora-Bold.ttf",
        "Lora-Italic.ttf",
        "Inter-BoldItalic.ttf",
    ] {
        let off = differs(wrong);
        eprintln!("{wrong}: {off} px differ, Lora-BoldItalic.ttf {right}");
        assert!(off > 3 * right, "{wrong}: {off} px differ, against {right}");
    }
    assert!(right * 10 < count, "{right} of {count} px differ");
}

#[test]
fn titles_export_their_own_size_colours_box_outline_and_place() {
    use openreel_engine::{Rgb, TextLook};
    let fixture = Fixture::new();
    let source = fixture.video(
        "black.mp4",
        "color=c=black:s=1280x720:r=10:d=2",
        "null",
        None,
    );
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 1.5, 0.0);
    let red = Rgb(255, 0, 0);
    let looks = [
        // Red on a solid blue box, a quarter right and well up.
        (
            TitleStyle::Caption,
            1.0,
            TextLook {
                color: Some(red),
                box_color: Some(Rgb(0, 0, 255)),
                box_opacity: Some(1.0),
                x: 0.25,
                y: -0.4,
                ..TextLook::default()
            },
        ),
        // The same at half opacity: the box fades with the text.
        (
            TitleStyle::Caption,
            0.5,
            TextLook {
                color: Some(red),
                box_color: Some(Rgb(0, 0, 255)),
                box_opacity: Some(1.0),
                x: 0.25,
                y: -0.4,
                ..TextLook::default()
            },
        ),
        // A green outline where the style has a black one.
        (
            TitleStyle::MinimalCaption,
            1.0,
            TextLook {
                outline_color: Some(Rgb(0, 255, 0)),
                outline: Some(0.15),
                ..TextLook::default()
            },
        ),
        // No box, then the same twice as large.
        (
            TitleStyle::Caption,
            1.0,
            TextLook {
                color: Some(red),
                box_opacity: Some(0.0),
                ..TextLook::default()
            },
        ),
        (
            TitleStyle::Caption,
            1.0,
            TextLook {
                color: Some(red),
                box_opacity: Some(0.0),
                size: 2.0,
                ..TextLook::default()
            },
        ),
    ];
    for (index, (style, opacity, text)) in looks.into_iter().enumerate() {
        let id = timeline.add_clip("MMMM", ClipKind::Title, 0, index as f64 * 0.3, 0.3, 0.0);
        let title = timeline.clip_mut(id).unwrap();
        (title.title_style, title.opacity, title.text) = (style, opacity, text);
    }
    let decoded = frames(&fixture.export("styled.mp4", &timeline));
    assert_eq!(decoded.len(), 15);
    let frame = |index: usize| &decoded[index * 3 + 1];
    let is_red = |p: &[u8; 3]| p[0] > 200 && p[1] < 60 && p[2] < 60;

    let (text, letters) = pixel_box(frame(0), 1280, is_red);
    let (boxed, blue) = pixel_box(frame(0), 1280, |p| p[2] > 200 && p[0] < 60 && p[1] < 60);
    eprintln!("moved: text {text:?} ({letters} px), box {boxed:?} ({blue} px)");
    assert!(letters > 300 && blue > 3000);
    let centre = (text[0] + text[2]) as f64 / 2.0;
    assert!(
        (centre - 960.0).abs() < 20.0,
        "a quarter right of centre: {centre}"
    );
    // The caption's line box top, 90 px up, moved 40% of 720 up; an M's
    // flat top is Inter's cap height, 0.7275 em, under its ascender,
    // ceil(0.96875 x 38) = 37 px down the line box.
    assert!(
        (text[1] as f64 - (720.0 - 90.0 - 288.0 + 37.0 - 0.7275 * 38.0)).abs() <= 2.0,
        "{text:?}"
    );
    assert!(
        boxed[0] < text[0] && boxed[1] < text[1] && boxed[2] > text[2] && boxed[3] > text[3],
        "the box surrounds the text"
    );
    let no_white = |f: &[u8]| pixel_box(f, 1280, |p| p.iter().all(|&v| v > 200)).1 == 0;
    assert!(no_white(frame(0)), "the text is red, not the style's white");

    // Half opacity: the solid blue box is half blue over black.
    let corner = (boxed[1] + 3) * 1280 + boxed[0] + 3;
    let faded = &frame(1)[corner * 3..corner * 3 + 3];
    assert!(
        (100..=160).contains(&faded[2]) && faded[0] < 40,
        "{faded:?}"
    );

    let is_green = |p: &[u8; 3]| p[1] > 200 && p[0] < 80 && p[2] < 80;
    let (_, outlined) = pixel_box(frame(2), 1280, is_green);
    assert!(outlined > 500, "a green outline: {outlined} px");
    assert!(
        pixel_box(frame(3), 1280, is_green).1 == 0,
        "only where it was set"
    );

    let (small, _) = pixel_box(frame(3), 1280, is_red);
    let (large, _) = pixel_box(frame(4), 1280, is_red);
    let height = |b: [usize; 4]| (b[3] - b[1]) as f64;
    let ratio = height(large) / height(small);
    eprintln!("size 1 {small:?}, size 2 {large:?}, ratio {ratio:.2}");
    assert!((1.8..=2.2).contains(&ratio), "twice as tall: {ratio}");
    assert!(
        large[3] <= small[3] + 2,
        "it grows upward: {small:?} {large:?}"
    );
    assert!(no_white(frame(3)) && no_white(frame(4)), "and has no box");
}

/// A shadow falls `distance` x the font size along its angle: at 0.4 of
/// a 38 px caption and 30 degrees, 15.2 x (cos 30, sin 30) = (13.2, 7.6),
/// so its ink ends 13 px right of and 8 px below the letters'. White
/// letters and a black shadow on grey keep every edge in luma, which
/// H.264 does not subsample.
#[test]
fn a_title_shadow_falls_where_its_angle_and_distance_put_it() {
    use openreel_engine::{Rgb, TextLook};
    let fixture = Fixture::new();
    let source = fixture.video("grey.mp4", "color=c=gray:s=1280x720:r=10:d=2", "null", None);
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 1.5, 0.0);
    let shadow = TextLook {
        box_opacity: Some(0.0),
        shadow_color: Some(Rgb::BLACK),
        shadow_opacity: Some(1.0),
        shadow_distance: 0.4,
        shadow_angle: 30.0,
        shadow_blur: 0.0,
        ..TextLook::default()
    };
    let looks = [
        // Sharp, then at half opacity, then soft.
        shadow,
        TextLook {
            shadow_opacity: Some(0.5),
            ..shadow
        },
        TextLook {
            shadow_blur: 0.2,
            ..shadow
        },
        // Under a white box, 19 px straight right: past the box's right
        // edge (the line's advance, about 3 px past the ink, plus 12 px of
        // padding), where drawtext alone cuts a shadow off.
        TextLook {
            color: Some(Rgb::BLACK),
            box_color: Some(Rgb::WHITE),
            box_opacity: Some(1.0),
            shadow_color: Some(Rgb(255, 0, 0)),
            shadow_distance: 0.5,
            shadow_angle: 0.0,
            ..shadow
        },
        // A 6 px white outline (0.15 x 38 = 5.7) casts a shadow too.
        TextLook {
            outline: Some(0.15),
            outline_color: Some(Rgb::WHITE),
            shadow_blur: 0.05,
            ..shadow
        },
    ];
    for (index, text) in looks.into_iter().enumerate() {
        let id = timeline.add_clip("MMMM", ClipKind::Title, 0, index as f64 * 0.3, 0.3, 0.0);
        let title = timeline.clip_mut(id).unwrap();
        (title.title_style, title.text) = (TitleStyle::Caption, text);
    }
    let decoded = frames(&fixture.export("shadow.mp4", &timeline));
    let frame = |index: usize| &decoded[index * 3 + 1];
    let white = |p: &[u8; 3]| p.iter().all(|&v| v > 200);
    let darker = |by: u8| move |p: &[u8; 3]| p[0] < 128 - by;
    let near = |got: usize, want: usize, by: usize| got.abs_diff(want) <= by;

    let (letters, count) = pixel_box(frame(0), 1280, white);
    let (shade, shaded) = pixel_box(frame(0), 1280, |p| p[0] < 40);
    eprintln!("sharp: letters {letters:?} ({count} px), shadow {shade:?} ({shaded} px)");
    assert!(count > 300 && shaded > 300);
    assert!(
        near(shade[2], letters[2] + 13, 1)
            && near(shade[3], letters[3] + 8, 1)
            && near(shade[1], letters[1] + 8, 1),
        "13 right and 8 down: letters {letters:?}, shadow {shade:?}"
    );
    let (edge, _) = pixel_box(frame(0), 1280, darker(10));
    assert!(edge[2] <= letters[2] + 14, "a sharp edge: {edge:?}");

    let darkest = |f: &[u8]| f.as_chunks::<3>().0.iter().map(|p| p[0]).min().unwrap();
    // The shadow's typical pixel, away from the letters' edges, where the
    // codec rings a few levels past it.
    let typical = |f: &[u8]| {
        let mut dark: Vec<u8> = f
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| p[0])
            .filter(|&v| v < 110)
            .collect();
        dark.sort_unstable();
        dark[dark.len() / 2]
    };
    let half = typical(frame(1));
    eprintln!("half: typical {half}, darkest {}", darkest(frame(1)));
    assert!((56..=72).contains(&half), "half of 128 dark: {half}");

    let (soft, _) = pixel_box(frame(2), 1280, darker(10));
    let (letters, _) = pixel_box(frame(2), 1280, white);
    let soft_darkest = darkest(frame(2));
    eprintln!("soft: letters {letters:?}, shadow {soft:?}, darkest {soft_darkest}");
    assert!(
        soft[2] >= letters[2] + 13 + 7 && soft[1] + 3 <= letters[1] + 8,
        "a 7.6 px blur spreads it: letters {letters:?}, shadow {soft:?}"
    );
    assert!(soft_darkest > 30, "and never reaches black: {soft_darkest}");

    let is_white = |p: &[u8; 3]| p.iter().all(|&v| v > 220);
    let is_red = |p: &[u8; 3]| p[0] > 150 && p[1] < 90 && p[2] < 90;
    let (boxed, _) = pixel_box(frame(3), 1280, is_white);
    let (red, reds) = pixel_box(frame(3), 1280, is_red);
    eprintln!("boxed: box {boxed:?}, shadow {red:?} ({reds} px)");
    assert!(reds > 200, "a red shadow over the box: {reds} px");
    assert!(
        red[2] >= boxed[2] + 2,
        "it runs past the box's edge: box {boxed:?}, shadow {red:?}"
    );

    let (outlined, _) = pixel_box(frame(4), 1280, white);
    let (cast, _) = pixel_box(frame(4), 1280, darker(64));
    eprintln!("outlined: letters {outlined:?}, shadow {cast:?}");
    assert!(
        (outlined[2] + 11..=outlined[2] + 16).contains(&cast[2]),
        "the outline's shadow too: letters {outlined:?}, shadow {cast:?}"
    );
}

/// A keyed title's size, place and opacity, with an outline, a soft shadow
/// over a box, and a highlight caption's lit words, draw on each frame as a
/// plain title holding that frame's values does.
#[test]
fn keyed_titles_draw_each_frame_as_a_plain_title_does() {
    use openreel_engine::{Ease, Rgb, TextLook};
    let fixture = Fixture::new();
    let source = fixture.video("grey.mp4", "color=c=gray:s=1280x720:r=10:d=3", "null", None);
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 3.0, 0.0);
    let outlined = TextLook {
        outline: Some(0.08),
        outline_color: Some(Rgb::BLACK),
        ..TextLook::default()
    };
    let soft = TextLook {
        box_opacity: Some(0.5),
        shadow_color: Some(Rgb(200, 0, 0)),
        shadow_opacity: Some(0.8),
        shadow_distance: 0.2,
        shadow_blur: 0.15,
        ..TextLook::default()
    };
    let sharp = TextLook {
        shadow_color: Some(Rgb::BLACK),
        shadow_opacity: Some(1.0),
        shadow_blur: 0.0,
        ..TextLook::default()
    };
    let titles = [
        ("Keyed MOVE", TitleStyle::Caption, outlined, 0.0),
        ("Soft SHADOW", TitleStyle::LowerThird, soft, 1.0),
        (
            "LEFTWORD RIGHTWORD",
            TitleStyle::HighlightCaption,
            sharp,
            2.0,
        ),
    ];
    let keys = [
        &[
            (Property::Scale, 0.0, 0.6, Ease::Linear),
            (Property::Scale, 1.0, 1.6, Ease::Linear),
            (Property::X, 0.0, -0.2, Ease::Linear),
            (Property::X, 1.0, 0.15, Ease::Linear),
            (Property::Opacity, 0.0, 0.3, Ease::EaseIn),
            (Property::Opacity, 1.0, 1.0, Ease::Linear),
        ][..],
        &[
            (Property::Scale, 0.0, 1.5, Ease::EaseInOut),
            (Property::Scale, 1.0, 0.7, Ease::Linear),
            (Property::Y, 0.0, -0.3, Ease::Linear),
            (Property::Y, 1.0, 0.0, Ease::Linear),
        ][..],
        &[
            (Property::Scale, 0.0, 0.8, Ease::Linear),
            (Property::Scale, 1.0, 1.4, Ease::Linear),
            (Property::Opacity, 0.0, 0.5, Ease::Linear),
            (Property::Opacity, 1.0, 1.0, Ease::Linear),
        ][..],
    ];
    let mut ids = Vec::new();
    for ((name, style, text, start), keys) in titles.into_iter().zip(keys) {
        let id = timeline.add_clip(name, ClipKind::Title, 0, start, 1.0, 0.0);
        let title = timeline.clip_mut(id).unwrap();
        (title.title_style, title.text) = (style, text);
        if style == TitleStyle::HighlightCaption {
            title.words = vec![0.0, 0.5];
        }
        for &(property, at, value, ease) in keys {
            title.keyframes.set(property, at, value);
            title.keyframes.ease_key(property, at, ease);
        }
        ids.push(id);
    }
    let keyed = frames(&fixture.export("keyed-titles.mp4", &timeline));
    // The same frames, each with a title of its own holding that frame's
    // size, place and opacity.
    let mut plain = Timeline::new(10.0);
    video(&mut plain, &source, 0, 0.0, 3.0, 0.0);
    for n in 0..30 {
        let at = n as f64 / 10.0;
        let clip = timeline.clip(ids[n / 10]).unwrap();
        let id = plain.add_clip(&clip.name, ClipKind::Title, 0, at, 0.1, at - clip.start);
        let title = plain.clip_mut(id).unwrap();
        title.title_style = clip.title_style;
        title.words = clip.words.clone();
        title.text = clip.text_at(at);
        title.opacity = clip.opacity_at(at);
    }
    let plain = frames(&fixture.export("plain-titles.mp4", &plain));
    assert_eq!((keyed.len(), plain.len()), (30, 30));
    // The codec puts a few hundred values more than 24 levels off around
    // the letters' edges; a frame drawn with the frame before's values puts
    // over 16,000 off.
    for n in 0..30 {
        let (a, b) = (&keyed[n], &plain[n]);
        let off = a
            .iter()
            .zip(b)
            .filter(|(a, b)| a.abs_diff(**b) > 24)
            .count();
        eprintln!("frame {n}: {off} values off by more than 24");
        assert!(
            off <= 2000,
            "frame {n}: {off} values differ by over 24 levels"
        );
    }
    // The keys did move and grow it: its box is over twice as wide at the
    // end as at the start, and further right.
    let dark = |p: &[u8; 3]| p.iter().all(|&v| v < 90);
    let (first, _) = pixel_box(&keyed[0], 1280, dark);
    let (last, _) = pixel_box(&keyed[9], 1280, dark);
    assert!(
        last[2] - last[0] > 2 * (first[2] - first[0]) && last[0] > first[0] + 100,
        "the box at the start {first:?} and the end {last:?}"
    );
}

#[test]
fn smooth_fades_follow_the_quarter_sine_in_preview_and_export() {
    // Halfway through a 1 s fade-in, a linear fade is at 0.5 and the
    // equal-power quarter sine at sin(pi/4) = 0.707 of full level.
    let fixture = Fixture::new();
    let source = fixture.color("tone.mp4", "black", Some(440));
    let rms = |samples: &[f32], from: f64, to: f64| {
        let part = &samples[(from * 48_000.0) as usize..(to * 48_000.0) as usize];
        (part.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / part.len() as f64).sqrt()
    };
    for (smooth, want) in [(false, 0.5), (true, std::f64::consts::FRAC_1_SQRT_2)] {
        let mut timeline = Timeline::new(30.0);
        let id = video(&mut timeline, &source, 0, 0.0, 2.0, 0.0);
        let clip = timeline.clip_mut(id).unwrap();
        clip.fade_in = 1.0;
        clip.smooth_fades = smooth;
        let exported = audio_samples(&fixture.export(&format!("fade-{smooth}.mp4"), &timeline));
        let export_mid = rms(&exported, 0.49, 0.51) / rms(&exported, 1.4, 1.6);

        let mut engine = openreel_media::AudioEngine::manual();
        engine.seek(0.0);
        engine.set_limit(2.0);
        engine.set_running(true);
        engine.set_voices(&[(
            id,
            openreel_media::AudioSegment {
                path: source.clone(),
                start: 0.0,
                duration: 2.0,
                source_in: 0.0,
                volume: 1.0,
                volume_keys: Vec::new(),
                fade_in: 1.0,
                fade_out: 0.0,
                duck: Vec::new(),
                noise: None,
                clean_voice: false,
                smooth_fades: smooth,
                speed: 1.0,
                crossfade: (0.0, 0.0),
            },
        )]);
        let mut preview = Vec::new();
        while preview.len() < 2 * 48_000 {
            let block = engine.render(1024);
            assert!(!block.is_empty(), "preview clock stopped");
            preview.extend(block.as_chunks::<2>().0.iter().map(|frame| frame[0]));
        }
        let preview_mid = rms(&preview, 0.49, 0.51) / rms(&preview, 1.4, 1.6);
        eprintln!(
            "smooth={smooth}: export {export_mid:.3}, preview {preview_mid:.3}, want {want:.3}"
        );
        assert!(
            (export_mid - want).abs() < 0.02,
            "export {export_mid} vs {want}"
        );
        assert!(
            (preview_mid - want).abs() < 0.02,
            "preview {preview_mid} vs {want}"
        );
    }
}

/// Volume keys at 1.0 then 0.25 a second later: halfway the sound is at
/// 0.625, and after the last key at 0.25, in preview and in export alike.
#[test]
fn keyed_volume_follows_its_keys_in_preview_and_export() {
    let fixture = Fixture::new();
    let source = fixture.color("tone.mp4", "black", Some(440));
    let rms = |samples: &[f32], from: f64, to: f64| {
        let part = &samples[(from * 48_000.0) as usize..(to * 48_000.0) as usize];
        (part.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / part.len() as f64).sqrt()
    };
    let mut timeline = Timeline::new(30.0);
    let id = video(&mut timeline, &source, 0, 0.0, 2.0, 0.0);
    let clip = timeline.clip_mut(id).unwrap();
    clip.keyframes.set(Property::Volume, 0.2, 1.0);
    clip.keyframes.set(Property::Volume, 1.2, 0.25);
    let keys = clip.keyframes.volume.clone();
    let exported = audio_samples(&fixture.export("volume-keys.mp4", &timeline));

    let mut engine = openreel_media::AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(2.0);
    engine.set_running(true);
    engine.set_voices(&[(
        id,
        openreel_media::AudioSegment {
            path: source.clone(),
            start: 0.0,
            duration: 2.0,
            source_in: 0.0,
            volume: 1.0,
            volume_keys: keys,
            fade_in: 0.0,
            fade_out: 0.0,
            duck: Vec::new(),
            noise: None,
            clean_voice: false,
            smooth_fades: false,
            speed: 1.0,
            crossfade: (0.0, 0.0),
        },
    )]);
    let mut preview = Vec::new();
    while preview.len() < 2 * 48_000 {
        let block = engine.render(1024);
        assert!(!block.is_empty(), "preview clock stopped");
        preview.extend(block.as_chunks::<2>().0.iter().map(|frame| frame[0]));
    }
    for (name, samples) in [("export", &exported), ("preview", &preview)] {
        let full = rms(samples, 0.05, 0.15);
        let halfway = rms(samples, 0.69, 0.71) / full;
        let held = rms(samples, 1.5, 1.9) / full;
        eprintln!("{name}: halfway {halfway:.3}, held {held:.3}");
        assert!((halfway - 0.625).abs() < 0.02, "{name} halfway {halfway}");
        assert!((held - 0.25).abs() < 0.01, "{name} held {held}");
    }
}

/// Under a 1 s mix centred on the cut at 2 s, the 440 Hz clip plays on to
/// 2.5 s while it fades out and the 880 Hz clip starts at 1.5 s, half a
/// second before its in point, fading in: equal power, so each is at
/// sin(pi/4) of full level at the cut and sin(pi/8) a quarter second
/// either side of it. Preview and export agree.
#[test]
fn sound_crossfades_under_a_transition_in_preview_and_export() {
    let fixture = Fixture::new();
    let low = fixture.color("low.mp4", "red", Some(440));
    let high = fixture.color("high.mp4", "blue", Some(880));
    let mut timeline = Timeline::new(30.0);
    let from = video(&mut timeline, &low, 0, 0.0, 2.0, 0.0);
    let to = video(&mut timeline, &high, 0, 2.0, 1.0, 1.0);
    timeline.clip_mut(to).unwrap().transition = Some(Transition::new(TransitionKind::Mix, 1.0));
    let exported = fixture.export("crossfade.mp4", &timeline);

    let crossfades = timeline.crossfades();
    let mut engine = openreel_media::AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(3.0);
    engine.set_running(true);
    let voices: Vec<_> = [from, to]
        .iter()
        .map(|&id| {
            let cross = crossfades[&id];
            let clip = timeline.clip(id).unwrap().sounding(cross);
            let segment = openreel_media::AudioSegment {
                path: clip.source.clone().unwrap(),
                start: clip.start,
                duration: clip.duration,
                source_in: clip.source_in,
                volume: 1.0,
                volume_keys: Vec::new(),
                fade_in: 0.0,
                fade_out: 0.0,
                duck: Vec::new(),
                noise: None,
                clean_voice: false,
                smooth_fades: false,
                speed: 1.0,
                crossfade: (cross.fade_in, cross.fade_out),
            };
            (id, segment)
        })
        .collect();
    engine.set_voices(&voices);
    let preview_path = fixture.0.join("preview.wav");
    let mut preview = Vec::new();
    while preview.len() < 3 * 48_000 {
        let block = engine.render(1024);
        assert!(!block.is_empty(), "preview clock stopped");
        preview.extend(block.as_chunks::<2>().0.iter().map(|frame| frame[0]));
    }
    let raw = fixture.0.join("preview.f32");
    std::fs::write(
        &raw,
        preview
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    run(Command::new("ffmpeg").args([
        "-y",
        "-v",
        "error",
        "-f",
        "f32le",
        "-ar",
        "48000",
        "-ac",
        "1",
        "-i",
        raw.to_str().unwrap(),
        preview_path.to_str().unwrap(),
    ]));

    let quarter = (std::f64::consts::PI / 8.0).sin();
    let half = std::f64::consts::FRAC_1_SQRT_2;
    // Windows of 18 cycles of 440 Hz (and 36 of 880), so neither tone
    // leaks into the other's measure.
    let around = |centre: f64| (centre - 9.0 / 440.0, centre + 9.0 / 440.0);
    for (name, path) in [("export", &exported), ("preview", &preview_path)] {
        let level = |hz, (from, to)| tone_window(path, hz, from, to);
        let (low_full, high_full) = (level(440.0, (0.5, 1.0)), level(880.0, (2.6, 2.9)));
        for (hz, full, at, want) in [
            (440.0, low_full, 2.0, half),
            (880.0, high_full, 2.0, half),
            (440.0, low_full, 2.25, quarter),
            (880.0, high_full, 1.75, quarter),
            (440.0, low_full, 2.75, 0.0),
            (880.0, high_full, 1.25, 0.0),
        ] {
            let got = level(hz, around(at)) / full;
            assert!(
                (got - want).abs() < 0.02,
                "{name}: {hz} Hz at {at} s is {got:.3} of full, want {want:.3}"
            );
        }
    }
}

#[test]
fn clean_voice_cuts_rumble_and_preview_matches_export() {
    // Loud and quiet 220 Hz "speech" every half second over 30 Hz rumble.
    let fixture = Fixture::new();
    let speech = fixture.0.join("speech.wav");
    run(Command::new("ffmpeg").args([
        "-y",
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=if(lt(mod(t\\,1)\\,0.5)\\,0.5\\,0.06)*sin(2*PI*220*t)+0.15*sin(2*PI*30*t):s=48000:d=3",
        "-c:a",
        "pcm_f32le",
        speech.to_str().unwrap(),
    ]));
    let speech = speech.to_str().unwrap().to_owned();
    let picture = fixture.color("black.mp4", "black", None);
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &picture, 0, 0.0, 3.0, 0.0);
    let voice = timeline.add_media_clip("voice", ClipKind::Audio, 1, 0.0, 3.0, 0.0, &speech);
    let plain = fixture.export("plain.mp4", &timeline);
    timeline.clip_mut(voice).unwrap().clean_voice = true;
    let cleaned = fixture.export("cleaned.mp4", &timeline);

    // Rumble falls by well over 12 dB; the speech tone keeps its level.
    let rumble = tone_window(&cleaned, 30.0, 1.6, 1.9) / tone_window(&plain, 30.0, 1.6, 1.9);
    assert!(rumble < 0.25, "rumble ratio {rumble}");
    let quiet = tone_window(&cleaned, 220.0, 1.6, 1.9) / tone_window(&plain, 220.0, 1.6, 1.9);
    let loud = tone_window(&cleaned, 220.0, 1.1, 1.4) / tone_window(&plain, 220.0, 1.1, 1.4);
    // Quiet speech gains the makeup; loud speech is pulled down toward it.
    assert!(quiet / loud > 1.6, "quiet {quiet} vs loud {loud}");

    // The preview runs the same high-pass and acompressor law: every 100 ms
    // window, including recovery after loud speech, matches export. A lone
    // mono clip exports mono; the preview upmixes it 3 dB down per channel,
    // as FFmpeg does, and must still compress it at the source's level.
    let mut engine = openreel_media::AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(3.0);
    engine.set_running(true);
    engine.set_voices(&[(
        voice,
        openreel_media::AudioSegment {
            path: speech.clone(),
            start: 0.0,
            duration: 3.0,
            source_in: 0.0,
            volume: 1.0,
            volume_keys: Vec::new(),
            fade_in: 0.0,
            fade_out: 0.0,
            duck: Vec::new(),
            noise: None,
            clean_voice: true,
            smooth_fades: false,
            speed: 1.0,
            crossfade: (0.0, 0.0),
        },
    )]);
    let mut preview = Vec::new();
    while preview.len() < 3 * 48_000 {
        let block = engine.render(1024);
        assert!(!block.is_empty(), "preview clock stopped");
        preview.extend(block.as_chunks::<2>().0.iter().map(|frame| frame[0]));
    }
    let exported = audio_samples(&cleaned);
    let db = |samples: &[f32]| {
        let mean =
            samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64;
        10.0 * mean.log10()
    };
    let mut worst = 0.0_f64;
    for window in 2..29 {
        let (a, b) = (window * 4800, (window + 1) * 4800);
        eprintln!(
            "w{window}: preview {:.2} export {:.2}",
            db(&preview[a..b]),
            db(&exported[a..b])
        );
        let upmix = 20.0 * std::f64::consts::FRAC_1_SQRT_2.log10();
        worst = worst.max((db(&preview[a..b]) - upmix - db(&exported[a..b])).abs());
    }
    eprintln!("clean voice: rumble x{rumble:.3}, quiet/loud {:.2}, worst preview-export window {worst:.2} dB", quiet / loud);
    assert!(worst < 0.1, "preview and export differ by {worst:.2} dB");
}

/// Noise reduction quiets steady hiss between words, keeps the words at
/// their level and in step with the picture (afftdn's output starts 25 ms
/// late), and plays in the preview as export renders it.
#[test]
fn noise_reduction_quiets_hiss_in_step_and_preview_matches_export() {
    // 220 Hz "words" for 0.3 s of every 0.5 s over white noise at -38.8 dBFS.
    let fixture = Fixture::new();
    let speech = fixture.0.join("hiss.wav");
    run(Command::new("ffmpeg").args([
        "-y",
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=if(lt(mod(t\\,0.5)\\,0.3)\\,0.3*sin(2*PI*220*t)\\,0)+0.02*(2*random(0)-1):s=48000:d=4",
        "-c:a",
        "pcm_f32le",
        speech.to_str().unwrap(),
    ]));
    let speech = speech.to_str().unwrap().to_owned();

    // The quietest stretches are the hiss: its level, and flat as white.
    let profile = openreel_media::measure_noise(&speech).unwrap();
    let hiss = 20.0 * (0.02 / 3f32.sqrt()).log10();
    assert!(
        (profile.floor - hiss).abs() < 1.5,
        "measured {profile:?}, hiss at {hiss:.1} dBFS"
    );
    assert!(
        profile.bands.iter().all(|band| band.abs() < 2.0),
        "white noise measured as {profile:?}"
    );

    let picture = fixture.color("black.mp4", "black", None);
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &picture, 0, 0.0, 4.0, 0.0);
    let voice = timeline.add_media_clip("voice", ClipKind::Audio, 1, 0.0, 4.0, 0.0, &speech);
    let plain = fixture.export("plain.mp4", &timeline);
    timeline.clip_mut(voice).unwrap().reduce_noise = true;
    // Export measures the source itself: the options carry no profile.
    let reduced = fixture.export("reduced.mp4", &timeline);
    let (plain, reduced) = (audio_samples(&plain), audio_samples(&reduced));
    let db = |samples: &[f32]| {
        let mean =
            samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64;
        10.0 * mean.log10()
    };
    let at = |seconds: f64| (seconds * 48_000.0) as usize;
    let gap = at(1.35)..at(1.45);
    let quieter = db(&plain[gap.clone()]) - db(&reduced[gap]);
    assert!(quieter > 8.0, "the hiss between words fell {quieter:.1} dB");
    let word = at(1.05)..at(1.25);
    let change = db(&reduced[word.clone()]) - db(&plain[word]);
    assert!(change.abs() < 0.5, "the words changed by {change:.2} dB");
    // In step: the reduced sound lines up with the plain one at no lag.
    // A 25 ms lag lands on a later word edge, so it cannot hide in the
    // tone's own period.
    let lag = (-1500_i64..=1500)
        .max_by(|a, b| {
            let score = |lag: i64| {
                (at(0.9)..at(1.9))
                    .map(|i| f64::from(plain[i]) * f64::from(reduced[(i as i64 + lag) as usize]))
                    .sum::<f64>()
            };
            score(*a).total_cmp(&score(*b))
        })
        .unwrap();
    assert!(
        lag.abs() < 20,
        "the reduced sound is {lag} samples off the plain one"
    );

    // The preview runs the same afftdn; a mono source is raised to its own
    // level around it, as export runs it on the mono file.
    let mut engine = openreel_media::AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(4.0);
    engine.set_running(true);
    engine.set_voices(&[(
        voice,
        openreel_media::AudioSegment {
            path: speech.clone(),
            start: 0.0,
            duration: 4.0,
            source_in: 0.0,
            volume: 1.0,
            volume_keys: Vec::new(),
            fade_in: 0.0,
            fade_out: 0.0,
            duck: Vec::new(),
            noise: Some(profile),
            clean_voice: false,
            smooth_fades: false,
            speed: 1.0,
            crossfade: (0.0, 0.0),
        },
    )]);
    let mut preview = Vec::new();
    while preview.len() < 4 * 48_000 {
        let block = engine.render(1024);
        assert!(!block.is_empty(), "preview clock stopped");
        preview.extend(block.as_chunks::<2>().0.iter().map(|frame| frame[0]));
    }
    // Every 100 ms window matches. AAC takes some of the reduced hiss near
    // -51 dBFS away: 0.15-0.25 dB with FFmpeg 9.0.2, up to 0.66 dB with 9.1,
    // while FFmpeg's own afftdn written uncompressed matched the preview
    // within 0.04 dB (9.0.2) and a millionth of a dB (9.1). Those windows
    // get 1 dB; a preview without the measured profile is about 10 dB off.
    let upmix = 20.0 * std::f64::consts::FRAC_1_SQRT_2.log10();
    let (mut words, mut gaps) = (0.0_f64, 0.0_f64);
    for window in 2..38 {
        let (a, b) = (window * 4800, (window + 1) * 4800);
        let exported = db(&reduced[a..b]);
        let diff = (db(&preview[a..b]) - upmix - exported).abs();
        if exported > -30.0 {
            words = words.max(diff);
        } else {
            gaps = gaps.max(diff);
        }
    }
    eprintln!(
        "noise reduction: hiss -{quieter:.1} dB, words {change:+.2} dB, lag {lag}, \
         preview against export {words:.2} dB in words, {gaps:.2} dB between"
    );
    assert!(
        words < 0.05,
        "preview and export words differ by {words:.2} dB"
    );
    assert!(
        gaps < 1.0,
        "preview and export hiss differs by {gaps:.2} dB"
    );
}

#[test]
fn transparent_first_title_hides_its_box_and_blocks_later_titles() {
    let fixture = Fixture::new();
    let source = fixture.color("white.mp4", "white", None);
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 0, 0.0, 0.3, 0.0);
    let id = timeline.add_clip("INVISIBLE", ClipKind::Title, 1, 0.0, 0.3, 0.0);
    timeline.clip_mut(id).unwrap().opacity = 0.0;
    timeline.add_clip("HIDDEN TOO", ClipKind::Title, 0, 0.0, 0.3, 0.0);
    let exported = fixture.export("transparent-title.mp4", &timeline);
    assert_frames(
        "transparent selected title",
        &frames(&exported),
        &[original[0].as_slice(); 3],
    );
}

fn tone_amplitude(path: &Path, frequency: f64) -> f64 {
    tone_window(path, frequency, 0.1, 0.9)
}

/// Amplitude of `frequency` over output seconds `[from, to)`.
fn tone_window(path: &Path, frequency: f64, from: f64, to: f64) -> f64 {
    let out = run(Command::new("ffmpeg").args([
        "-v",
        "error",
        "-i",
        path.to_str().unwrap(),
        "-vn",
        "-ar",
        "48000",
        "-ac",
        "1",
        "-f",
        "f32le",
        "-",
    ]));
    let samples: Vec<_> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes) as f64)
        .collect();
    let window = &samples[(from * 48000.0) as usize..(to * 48000.0) as usize];
    let (mut sin, mut cos) = (0.0, 0.0);
    for (i, sample) in window.iter().enumerate() {
        let angle = std::f64::consts::TAU * frequency * i as f64 / 48000.0;
        sin += sample * angle.sin();
        cos += sample * angle.cos();
    }
    2.0 * sin.hypot(cos) / window.len() as f64
}

#[test]
fn hidden_video_audio_is_mixed_and_detachment_is_still_honored() {
    let fixture = Fixture::new();
    let red = fixture.color("red-tone.mp4", "red", Some(440));
    let blue = fixture.color("blue-tone.mp4", "blue", Some(880));
    let original = frames(Path::new(&red));
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &red, 0, 0.0, 1.0, 0.0);
    let hidden = video(&mut timeline, &blue, 1, 0.0, 1.0, 0.0);
    let mixed = fixture.export("mixed.mp4", &timeline);
    let a = tone_amplitude(&mixed, 440.0);
    let b = tone_amplitude(&mixed, 880.0);
    eprintln!("mixed audio amplitudes: visible 440 Hz={a}, hidden 880 Hz={b}");
    assert!(
        a > 0.08 && b > 0.08,
        "both original sine tones must survive"
    );
    timeline.clip_mut(hidden).unwrap().embedded_audio = false;
    let detached = fixture.export("detached.mp4", &timeline);
    let a = tone_amplitude(&detached, 440.0);
    let b = tone_amplitude(&detached, 880.0);
    eprintln!("detached audio amplitudes: visible 440 Hz={a}, hidden 880 Hz={b}");
    assert!(
        a > 0.08 && b < 0.002,
        "detached embedded audio must be absent"
    );
    assert_frames(
        "hidden-video audio does not change picture",
        &frames(&mixed),
        &[original[0].as_slice(); 10],
    );
    assert_frames(
        "detached audio does not change picture",
        &frames(&detached),
        &[original[0].as_slice(); 10],
    );
}

#[test]
fn audio_coalescing_preserves_overlaps_and_gain_changes() {
    let fixture = Fixture::new();
    let source = fixture.color("tone.mp4", "red", Some(440));
    for overlap in [true, false] {
        let mut timeline = Timeline::new(30.0);
        let picture = video(&mut timeline, &source, 0, 0.0, 1.0, 0.0);
        timeline.clip_mut(picture).unwrap().embedded_audio = false;
        for n in 0..2 {
            let (start, duration, gain) = if overlap {
                (0.0, 1.0, 0.25)
            } else {
                (n as f64 * 0.5, 0.5, if n == 0 { 0.25 } else { 0.75 })
            };
            let id = timeline.add_media_clip(
                "sound",
                ClipKind::Audio,
                n,
                start,
                duration,
                start,
                &source,
            );
            timeline.clip_mut(id).unwrap().volume = gain;
        }
        let exported = fixture.export(&format!("audio-overlap-{overlap}.mp4"), &timeline);
        let amplitude = tone_amplitude(&exported, 440.0);
        eprintln!("overlap={overlap}, 440 Hz amplitude={amplitude}, expected about 0.0625");
        assert!(
            (0.05..0.075).contains(&amplitude),
            "do not merge overlaps or differing gains"
        );
    }
}

#[test]
fn exported_fades_ramp_the_clip_sound() {
    let fixture = Fixture::new();
    let source = fixture.color("tone.mp4", "red", Some(440));
    // Full 0.25 gain measures about 0.0625 (see the coalescing test). A
    // fade-out across the whole second averages half of that over the
    // measured 0.1-0.9 s window; a no-op or reversed fade would not.
    let mut levels = Vec::new();
    for (fade_in, fade_out) in [(0.0, 0.0), (0.0, 1.0), (5.0, 0.0)] {
        let mut timeline = Timeline::new(30.0);
        let picture = video(&mut timeline, &source, 0, 0.0, 1.0, 0.0);
        let clip = timeline.clip_mut(picture).unwrap();
        clip.fade_in = fade_in;
        clip.fade_out = fade_out;
        let exported = fixture.export(&format!("fade-{fade_in}-{fade_out}.mp4"), &timeline);
        let early = tone_window(&exported, 440.0, 0.1, 0.3);
        let late = tone_window(&exported, 440.0, 0.7, 0.9);
        eprintln!("fade {fade_in}/{fade_out}: early={early} late={late}");
        levels.push((early, late));
    }
    let (full, out, overlong_in) = (levels[0], levels[1], levels[2]);
    assert!((full.0 - full.1).abs() < 0.1 * full.0, "unfaded is steady");
    // Fade-out: 0.8 of full early, 0.2 late (linear, centered windows).
    assert!(
        (out.0 / full.0 - 0.8).abs() < 0.08,
        "fade-out early {:?}",
        out
    );
    assert!(
        (out.1 / full.0 - 0.2).abs() < 0.08,
        "fade-out late {:?}",
        out
    );
    // A 5 s fade-in on a 1 s clip is fitted to 1 s: 0.2 early, 0.8 late.
    let r = (overlong_in.0 / full.0, overlong_in.1 / full.0);
    assert!(
        (r.0 - 0.2).abs() < 0.08 && (r.1 - 0.8).abs() < 0.08,
        "{r:?}"
    );
}

#[test]
fn encoder_failure_is_reported_without_success_progress() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices.mp4", "30");
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &source, 0, 0.0, 0.1, 0.0);
    let infos = [(source.clone(), probe(&source).unwrap())].into();
    let (tx, rx) = channel();
    let output = fixture.0.join("missing-directory/export.mp4");
    let error =
        export_timeline(&timeline, &infos, &output, &ExportOptions::default(), tx).unwrap_err();
    eprintln!("expected encoder failure: {error}");
    assert!(error.starts_with("export failed:"));
    assert!(!output.exists());
    assert!(!rx.try_iter().any(|progress| progress == 1.0));
}

#[test]
fn reordered_repeated_ranges_preserve_every_frame_across_a_long_edit() {
    let fixture = Fixture::new();
    let source = fixture.video(
        "reordered.mp4",
        "nullsrc=s=80x48:r=10:d=12",
        "geq=lum='40+160*mod(floor(N/pow(2,floor(X/8))),2)':cb=128:cr=128",
        Some(440),
    );
    let original = frames(Path::new(&source));
    let black = vec![0; original[0].len()];
    let mut timeline = Timeline::new(10.0);
    let mut expected = vec![black.as_slice(); 97];
    for (start, count, source_frame) in [
        (0, 6, 92),
        (6, 7, 13),
        (13, 4, 102),
        (17, 5, 31),
        (22, 8, 4),
        (30, 55, 32),
        (90, 7, 56),
    ] {
        video(
            &mut timeline,
            &source,
            0,
            start as f64 / 10.0,
            count as f64 / 10.0,
            source_frame as f64 / 10.0,
        );
        for offset in 0..count {
            expected[start + offset] = &original[source_frame + offset];
        }
    }
    let exported = fixture.export("reordered-export.mp4", &timeline);
    assert_frames(
        "reordered source clocks and late gap",
        &frames(&exported),
        &expected,
    );
    assert!(tone_amplitude(&exported, 440.0) > 0.06);
}

#[test]
fn normalized_export_brings_a_quiet_mix_to_the_loudness_target() {
    let fixture = Fixture::new();
    let source = fixture.color("quiet.mp4", "red", Some(440));
    let mut timeline = Timeline::new(30.0);
    let clip = video(&mut timeline, &source, 0, 0.0, 3.0, 0.0);
    timeline.clip_mut(clip).unwrap().volume = 0.1;

    let normalize = ExportOptions {
        normalize_loudness: true,
        ..Default::default()
    };
    let plain = fixture.export("quiet-plain.mp4", &timeline);
    let normalized = fixture.export_with("quiet-normalized.mp4", &timeline, &normalize);
    let raw = integrated_loudness(&plain);
    let loud = integrated_loudness(&normalized);
    eprintln!("plain {raw} LUFS, normalized {loud} LUFS");
    assert!(raw < -35.0, "the plain export keeps the quiet mix: {raw}");
    assert!((loud + 16.0).abs() < 1.0, "normalized to -16 LUFS: {loud}");
    let peak = tone_window(&normalized, 440.0, 0.1, 2.9);
    assert!(
        peak < 0.8,
        "the limiter keeps peaks under the ceiling: {peak}"
    );

    // A loud mix is turned down, not limited flat.
    timeline.clip_mut(clip).unwrap().volume = 2.0;
    let hot = integrated_loudness(&fixture.export_with("hot.mp4", &timeline, &normalize));
    assert!((hot + 16.0).abs() < 1.0, "loud mix normalized down: {hot}");

    // Without audio there is nothing to measure; export still succeeds.
    let mut silent = Timeline::new(30.0);
    let picture = video(&mut silent, &source, 0, 0.0, 1.0, 0.0);
    silent.clip_mut(picture).unwrap().embedded_audio = false;
    fixture.export_with("silent.mp4", &silent, &normalize);
}

#[test]
fn ducked_music_dips_only_while_the_speech_source_is_audible() {
    let fixture = Fixture::new();
    let talk = fixture.color("talk.mp4", "blue", Some(440));
    let bed = fixture.color("bed.mp4", "green", Some(880));
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &talk, 0, 0.0, 3.0, 0.0);
    let music = timeline.add_media_clip("bed", ClipKind::Audio, 1, 0.0, 3.0, 0.0, &bed);
    timeline.clip_mut(music).unwrap().duck = true;
    let options = ExportOptions {
        speech: [(talk.clone(), vec![(0.8, 1.4)])].into(),
        ..Default::default()
    };
    let ducked = fixture.export_with("ducked.mp4", &timeline, &options);
    // 0.2 s down before speech, 0.3 s held after it, 0.8 s back up.
    let before = tone_window(&ducked, 880.0, 0.1, 0.5);
    let during = tone_window(&ducked, 880.0, 0.95, 1.25);
    let release = tone_window(&ducked, 880.0, 2.05, 2.15);
    let after = tone_window(&ducked, 880.0, 2.55, 2.95);
    eprintln!("bed 880 Hz: before {before}, during {during}, release {release}, after {after}");
    assert!(
        (0.2..0.32).contains(&(during / before)),
        "ducked to a quarter"
    );
    // The preview's raised cosine in decibels: halfway back is -6 dB.
    assert!(
        (0.45..0.55).contains(&(release / before)),
        "halfway through the release: {}",
        release / before
    );
    assert!((after / before - 1.0).abs() < 0.1, "recovers after speech");
    let voice = tone_window(&ducked, 440.0, 0.95, 1.25);
    assert!(
        (voice / tone_window(&ducked, 440.0, 0.1, 0.5) - 1.0).abs() < 0.1,
        "speech is untouched"
    );

    // Music cut 0.1 s after the speech ends carries on the same curve: the
    // new piece starts held down and is halfway back at the same moment.
    let mut cut = timeline.clone();
    cut.split_at(music, 1.5).expect("split the bed");
    let cut = fixture.export_with("ducked-cut.mp4", &cut, &options);
    let held = tone_window(&cut, 880.0, 1.52, 1.68) / before;
    assert!((0.2..0.32).contains(&held), "held after the cut: {held}");
    let release = tone_window(&cut, 880.0, 2.05, 2.15) / before;
    assert!(
        (0.45..0.55).contains(&release),
        "halfway back after the cut: {release}"
    );

    // Without speech evidence nothing ducks.
    let plain = fixture.export("unducked.mp4", &timeline);
    let flat = tone_window(&plain, 880.0, 1.35, 1.65) / tone_window(&plain, 880.0, 0.1, 0.6);
    assert!((flat - 1.0).abs() < 0.1, "{flat}");
}

/// Integrated loudness in LUFS, measured by FFmpeg's EBU R128 meter.
fn integrated_loudness(path: &Path) -> f64 {
    let out = run(Command::new("ffmpeg").args([
        "-hide_banner",
        "-nostats",
        "-i",
        path.to_str().unwrap(),
        "-vn",
        "-af",
        "ebur128",
        "-f",
        "null",
        "-",
    ]));
    let log = String::from_utf8_lossy(&out.stderr);
    let summary = &log[log.rfind("Summary:").expect("ebur128 summary")..];
    let line = summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("I:"))
        .expect("integrated loudness line");
    line.trim().trim_end_matches("LUFS").trim().parse().unwrap()
}

#[test]
fn five_hundred_scene_montage_exports_every_frame_with_audio() {
    let fixture = Fixture::new();
    let source = fixture.video(
        "montage.mp4",
        "nullsrc=s=64x36:r=30:d=17",
        "geq=lum='40+160*mod(N,2)':cb=128:cr=128",
        Some(440),
    );
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(30.0);
    for n in 0..500 {
        video(
            &mut timeline,
            &source,
            0,
            n as f64 / 30.0,
            1.0 / 30.0,
            n as f64 / 30.0,
        );
    }
    let exported = fixture.export("montage-export.mp4", &timeline);
    let references: Vec<_> = original[..500].iter().map(Vec::as_slice).collect();
    assert_frames("500 scene montage", &frames(&exported), &references);
    assert!(
        tone_amplitude(&exported, 440.0) > 0.06,
        "montage embedded audio survives"
    );
}

#[test]
fn five_hundred_distinct_frames_with_unmergeable_audio_do_not_stall() {
    let fixture = Fixture::new();
    // Ten binary bars identify every frame uniquely, so even a multi-frame
    // shift cannot pass by matching a periodic solid-color fixture.
    let source = fixture.video(
        "distinct.mp4",
        "nullsrc=s=80x48:r=30:d=17",
        "geq=lum='40+160*mod(floor(N/pow(2,floor(X/8))),2)':cb=128:cr=128",
        Some(440),
    );
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(30.0);
    for n in 0..500 {
        let id = video(
            &mut timeline,
            &source,
            0,
            n as f64 / 30.0,
            1.0 / 30.0,
            n as f64 / 30.0,
        );
        // Different gains prevent audio-run coalescing. Video and audio must
        // independently advance, even with all 500 amix inputs present.
        timeline.clip_mut(id).unwrap().volume = if n % 2 == 0 { 0.5 } else { 1.0 };
    }
    let exported = fixture.export("distinct-export.mp4", &timeline);
    let expected: Vec<_> = original[..500].iter().map(Vec::as_slice).collect();
    assert_frames("500 unique frame identities", &frames(&exported), &expected);
    let amplitude = tone_amplitude(&exported, 440.0);
    eprintln!("500 unmergeable audio contributions: 440 Hz amplitude={amplitude}");
    assert!(amplitude > 0.04, "unmergeable embedded audio survives");
}

#[test]
fn segment_seams_keep_timestamps_and_titles_on_the_timeline_clock() {
    // Export renders bounded batches (eight seconds at most) as separate
    // segments. A fractional rate puts the seam at frame 240 inside a title.
    let fixture = Fixture::new();
    let fps = 30000.0 / 1001.0;
    let source = fixture.video(
        "black.mp4",
        "color=c=black:s=320x180:r=30000/1001:d=11",
        "null",
        Some(440),
    );
    let mut timeline = Timeline::new(fps);
    video(&mut timeline, &source, 0, 0.0, 10.0, 0.0);
    let title = timeline.add_clip("SEAM", ClipKind::Title, 1, 7.5, 1.0, 0.0);
    timeline.clip_mut(title).unwrap().title_style = TitleStyle::LowerThird;
    let exported = fixture.export("seam-export.mp4", &timeline);

    let count = (10.0 * fps).ceil() as usize;
    let titled = (7.5 * fps).ceil() as usize..(8.5 * fps).ceil() as usize;
    assert!(titled.contains(&240), "the title spans the first seam");
    let actual = frames(&exported);
    assert_eq!(actual.len(), count, "frame count across seams");
    for (n, frame) in actual.iter().enumerate() {
        let lit = frame.iter().filter(|&&value| value > 64).count();
        assert_eq!(
            lit > 0,
            titled.contains(&n),
            "frame {n}: title visibility ({lit} lit values)"
        );
    }
    let json = stream_json(&exported);
    let frames_json = json["frames"].as_array().unwrap();
    assert_eq!(frames_json.len(), count);
    for (n, frame) in frames_json.iter().enumerate() {
        let pts: f64 = frame["best_effort_timestamp_time"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            (pts - n as f64 / fps).abs() < 0.000001,
            "frame {n} at {pts}, expected {}",
            n as f64 / fps
        );
    }
    let samples = audio_samples(&exported);
    assert_eq!(samples.len(), 480_000, "audio keeps the timeline duration");
    let seam = (240.0 / fps * 48000.0) as usize;
    let quietest = samples[seam - 4800..seam + 4800]
        .chunks(480)
        .map(|chunk| chunk.iter().map(|s| s.abs()).fold(0.0_f32, f32::max))
        .fold(f32::MAX, f32::min);
    assert!(
        quietest > 0.02,
        "audio is continuous at the seam: {quietest}"
    );
}

#[test]
fn speed_shows_every_nth_source_frame_or_holds_each_longer() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices.mp4", "30");
    let original = frames(Path::new(&source));
    let mut timeline = Timeline::new(30.0);
    // Speed, source in (frames), output frames, and the source frame each
    // one shows: the frame covering `source_in + n / 30 * speed`. 2x and 4x
    // land exactly on source frames, as does every fourth frame at 0.25x.
    let cases: [(f64, f64, usize, &[usize]); 5] = [
        (2.0, 2.0, 5, &[2, 4, 6, 8, 10]),
        (0.5, 20.0, 6, &[20, 20, 21, 21, 22, 22]),
        (1.5, 0.0, 4, &[0, 1, 3, 4]),
        (4.0, 1.0, 4, &[1, 5, 9, 13]),
        (0.25, 26.0, 5, &[26, 26, 26, 26, 27]),
    ];
    let mut at = 0.0;
    let mut expected: Vec<&[u8]> = Vec::new();
    for (speed, source_in, count, shown) in cases {
        let duration = count as f64 / 30.0;
        let id = video(&mut timeline, &source, 0, at, duration, source_in / 30.0);
        timeline.clip_mut(id).unwrap().speed = speed;
        at += duration;
        expected.extend(shown.iter().map(|&frame| original[frame].as_slice()));
    }
    let exported = fixture.export("speeds.mp4", &timeline);
    assert_frames("speeds", &frames(&exported), &expected);
}

#[test]
fn speed_keeps_the_pitch_and_preview_matches_export() {
    // 440 Hz for the first 1.5 s of source, then 220 Hz.
    let fixture = Fixture::new();
    let tones = fixture.0.join("tones.wav");
    run(Command::new("ffmpeg").args([
        "-y",
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=0.5*sin(2*PI*if(lt(t\\,1.5)\\,440\\,220)*t):s=48000:d=3",
        "-c:a",
        "pcm_f32le",
        tones.to_str().unwrap(),
    ]));
    let tones = tones.to_str().unwrap().to_owned();
    let picture = fixture.color("black.mp4", "black", None);
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &picture, 0, 0.0, 3.0, 0.0);
    // Source 0-3 s at 2x fills 0-1.5 s: 440 Hz until 0.75 s. Source 1.2-2 s
    // at 0.4x (two chained stages) fills 1.5-3.5 s: 440 Hz until 2.25 s.
    let segments = [(0.0, 1.5, 0.0, 2.0), (1.5, 2.0, 1.2, 0.4)];
    let mut voices = Vec::new();
    for (start, duration, source_in, speed) in segments {
        let id = timeline.add_media_clip(
            "tones",
            ClipKind::Audio,
            1,
            start,
            duration,
            source_in,
            &tones,
        );
        timeline.clip_mut(id).unwrap().speed = speed;
        voices.push((
            id,
            openreel_media::AudioSegment {
                path: tones.clone(),
                start,
                duration,
                source_in,
                volume: 1.0,
                volume_keys: Vec::new(),
                fade_in: 0.0,
                fade_out: 0.0,
                duck: Vec::new(),
                noise: None,
                clean_voice: false,
                smooth_fades: false,
                speed,
                crossfade: (0.0, 0.0),
            },
        ));
    }
    let exported = audio_samples(&fixture.export("speeds.mp4", &timeline));

    let mut engine = openreel_media::AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(3.5);
    engine.set_running(true);
    engine.set_voices(&voices);
    let mut preview = Vec::new();
    while preview.len() < (3.5 * 48_000.0) as usize {
        let block = engine.render(1024);
        assert!(!block.is_empty(), "preview clock stopped");
        preview.extend(block.as_chunks::<2>().0.iter().map(|frame| frame[0]));
    }
    // The preview upmixes the mono source 3 dB down per channel.
    for sample in &mut preview {
        *sample *= std::f32::consts::SQRT_2;
    }

    let tone = |samples: &[f32], frequency: f64, from: f64, to: f64| {
        let window = &samples[(from * 48_000.0) as usize..(to * 48_000.0) as usize];
        let (mut sin, mut cos) = (0.0, 0.0);
        for (i, sample) in window.iter().enumerate() {
            let angle = std::f64::consts::TAU * frequency * i as f64 / 48_000.0;
            sin += f64::from(*sample) * angle.sin();
            cos += f64::from(*sample) * angle.cos();
        }
        2.0 * f64::hypot(sin, cos) / window.len() as f64
    };
    for (name, samples) in [("export", &exported), ("preview", &preview)] {
        // Each stretch is heard at its recorded pitch, never doubled or
        // lowered, and switches tone where the speed puts source 1.5 s.
        for (from, to, frequency, wrong) in [
            (0.1, 0.65, 440.0, 880.0),
            (0.85, 1.4, 220.0, 440.0),
            (1.6, 2.15, 440.0, 176.0),
            (2.35, 3.4, 220.0, 88.0),
        ] {
            let right = tone(samples, frequency, from, to);
            let shifted = tone(samples, wrong, from, to);
            eprintln!("{name} {from}-{to}s: {frequency} Hz {right:.3}, {wrong} Hz {shifted:.3}");
            assert!(
                right > 0.35 && shifted < 0.05,
                "{name} {from}-{to}s: {frequency} Hz at {right}, {wrong} Hz at {shifted}"
            );
        }
    }
    let db = |samples: &[f32]| {
        let mean =
            samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64;
        10.0 * mean.log10()
    };
    let mut worst = 0.0_f64;
    for window in 1..34 {
        let (a, b) = (window * 4800, (window + 1) * 4800);
        worst = worst.max((db(&preview[a..b]) - db(&exported[a..b])).abs());
    }
    eprintln!("speed: worst preview-export window {worst:.2} dB");
    assert!(worst < 0.5, "preview and export differ by {worst:.2} dB");
}

fn with(settings: ExportSettings) -> ExportOptions {
    ExportOptions {
        settings,
        ..ExportOptions::default()
    }
}

#[test]
fn another_frame_rate_samples_the_source_at_that_rate() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices60.mp4", "60");
    let original = frames(Path::new(&source));
    // A 30 fps timeline showing 0.2 s from source frame 6.
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &source, 0, 0.0, 0.2, 0.1);
    // Output frame n shows the source frame covering 0.1 + n / rate.
    for (fps, shown) in [
        (60, (6..18).collect::<Vec<_>>()),
        (24, vec![6, 8, 11, 13, 16]),
    ] {
        let settings = ExportSettings {
            fps: Some(fps),
            ..ExportSettings::default()
        };
        let name = format!("rate{fps}.mp4");
        let exported = fixture.export_with(&name, &timeline, &with(settings));
        assert_eq!(
            probe(exported.to_str().unwrap()).unwrap().fps,
            f64::from(fps)
        );
        let expected: Vec<&[u8]> = shown.iter().map(|&n| original[n].as_slice()).collect();
        assert_frames(&name, &frames(&exported), &expected);
    }
}

/// The share of the frame white text covers: left, top, right and bottom
/// as fractions of its width and height.
fn text_box(frame: &[u8], width: usize) -> [f64; 4] {
    let height = frame.len() / 3 / width;
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for (index, pixel) in frame.as_chunks::<3>().0.iter().enumerate() {
        if pixel.iter().all(|&value| value > 200) {
            let (x, y) = (index % width, index / width);
            (left, top) = (left.min(x), top.min(y));
            (right, bottom) = (right.max(x + 1), bottom.max(y + 1));
        }
    }
    assert!(right > left, "no text drawn");
    let (w, h) = (width as f64, height as f64);
    [
        left as f64 / w,
        top as f64 / h,
        right as f64 / w,
        bottom as f64 / h,
    ]
}

#[test]
fn a_larger_resolution_keeps_pictures_and_titles_in_place() {
    let fixture = Fixture::new();
    let quadrants = fixture.video(
        "quadrants.mp4",
        "nullsrc=s=320x180:r=10:d=1",
        "format=gbrp,geq=r='255*lt(X,W/2)':g='255*lt(Y,H/2)':b=64",
        None,
    );
    let blue = fixture.color("blue.mp4", "blue", None);
    let mut timeline = Timeline::new(10.0).with_format(Format::Landscape);
    video(&mut timeline, &quadrants, 0, 0.0, 0.3, 0.0);
    video(&mut timeline, &blue, 0, 0.3, 0.3, 0.0);
    let title = timeline.add_clip("Eddie", ClipKind::Title, 0, 0.3, 0.3, 0.0);
    timeline.clip_mut(title).unwrap().title_style = TitleStyle::LowerThird;

    let auto = fixture.export("auto.mp4", &timeline);
    let settings = ExportSettings {
        resolution: Resolution::P720,
        ..ExportSettings::default()
    };
    let large = fixture.export_with("720p.mp4", &timeline, &with(settings));
    let info = probe(large.to_str().unwrap()).unwrap();
    assert_eq!((info.width, info.height), (1280, 720));

    // The picture fills the larger frame: each quadrant four times as far in.
    let large_frames = frames(&large);
    let points = [(320, 180), (960, 180), (320, 540), (960, 540)];
    assert_colors(
        "720p",
        &colors_at(&large_frames[1], 1280, &points),
        &[TL, TR, BL, BR],
    );
    // The title covers the same share of the frame at both sizes.
    let small = text_box(&frames(&auto)[4], 320);
    let big = text_box(&large_frames[4], 1280);
    for (side, (a, b)) in small.iter().zip(big).enumerate() {
        assert!(
            (a - b).abs() < 0.02,
            "side {side}: {small:?} against {big:?}"
        );
    }
}

#[test]
fn higher_quality_keeps_more_in_a_larger_file() {
    let fixture = Fixture::new();
    let busy = fixture.video(
        "busy.mp4",
        "testsrc2=s=320x180:r=30:d=1",
        "noise=alls=30:allf=t",
        None,
    );
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &busy, 0, 0.0, 1.0, 0.0);
    let sizes: Vec<u64> = Quality::ALL
        .into_iter()
        .map(|quality| {
            let settings = ExportSettings {
                quality,
                ..ExportSettings::default()
            };
            let name = format!("{}.mp4", quality.as_str());
            let exported = fixture.export_with(&name, &timeline, &with(settings));
            std::fs::metadata(exported).unwrap().len()
        })
        .collect();
    assert!(sizes.windows(2).all(|pair| pair[0] < pair[1]), "{sizes:?}");
}

/// Each stream in a file as `type:codec`, and the file's length.
fn contents(path: &Path) -> (Vec<String>, f64) {
    let out = run(Command::new("ffprobe").args([
        "-v",
        "error",
        "-show_entries",
        "stream=codec_type,codec_name:format=duration",
        "-of",
        "json",
        path.to_str().unwrap(),
    ]));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("probe JSON");
    let streams = json["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stream| format!("{}:{}", stream["codec_type"], stream["codec_name"]).replace('"', ""))
        .collect();
    let length = json["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (streams, length)
}

#[test]
fn audio_only_writes_the_whole_mix_in_each_format_without_a_picture() {
    let fixture = Fixture::new();
    let tone = fixture.color("tone.mp4", "red", Some(440));
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &tone, 0, 0.0, 1.0, 0.0);
    // A title after the sound: the file still lasts the whole timeline.
    timeline.add_clip("End", ClipKind::Title, 0, 1.0, 0.5, 0.0);
    for (format, codec) in [
        (AudioFormat::Mp3, "mp3"),
        (AudioFormat::Wav, "pcm_s16le"),
        (AudioFormat::Aac, "aac"),
        (AudioFormat::Flac, "flac"),
    ] {
        let settings = ExportSettings {
            kind: ExportKind::Audio,
            audio: format,
            ..ExportSettings::default()
        };
        let name = format!("mix.{}", format.extension());
        let exported = fixture.export_with(&name, &timeline, &with(settings));
        let (streams, length) = contents(&exported);
        assert_eq!(streams, [format!("audio:{codec}")], "{name}");
        assert!((length - 1.5).abs() < 0.05, "{name} lasts {length} s");
        assert!(
            tone_window(&exported, 440.0, 0.1, 0.9) > 0.08,
            "{name}: the tone"
        );
        assert!(
            tone_window(&exported, 440.0, 1.1, 1.45) < 0.005,
            "{name}: silence under the title"
        );
    }
}

#[test]
fn hardware_encoding_joins_its_segments_or_falls_back_to_x264() {
    let fixture = Fixture::new();
    // Six files: more than one batch, so segments from separate encoder
    // runs are joined.
    let colours = [
        ("blue", BLUE),
        ("red", RED),
        ("green", GREEN),
        ("white", [255; 3]),
        ("yellow", [255, 255, 0]),
        ("cyan", [0, 255, 255]),
    ];
    let mut timeline = Timeline::new(30.0).with_format(Format::Landscape);
    let mut infos = HashMap::new();
    for (index, (name, _)) in colours.iter().enumerate() {
        let path = fixture.color(&format!("{name}.mp4"), name, Some(440));
        infos.insert(path.clone(), probe(&path).unwrap());
        video(&mut timeline, &path, 0, 0.3 * index as f64, 0.3, 0.0);
    }
    for hardware in [false, true] {
        let output = fixture.0.join(format!("hardware-{hardware}.mp4"));
        let options = ExportOptions {
            hardware_encode: hardware,
            ..ExportOptions::default()
        };
        let made = export_timeline(&timeline, &infos, &output, &options, channel().0).unwrap();
        let encoder = made.encoder.unwrap();
        // Without an NVIDIA GPU, as in CI, x264 encodes instead.
        assert!(
            encoder == "x264" || (hardware && encoder == "nvenc"),
            "{encoder}"
        );
        let decoded = run(Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&output)
            .args(["-f", "null", "-"]));
        assert!(
            decoded.stderr.is_empty(),
            "{encoder}: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let pictures = frames(&output);
        assert_eq!(pictures.len(), 54, "{encoder}: 1.8 s at 30 fps");
        for (index, (_, rgb)) in colours.iter().enumerate() {
            let shown = colors_at(&pictures[9 * index + 4], 320, &[(60, 40), (260, 140)]);
            assert_colors(encoder, &shown, &[*rgb; 2]);
        }
        assert!(
            tone_window(&output, 440.0, 0.1, 1.7) > 0.08,
            "{encoder}: the sound"
        );
    }
}

/// A GIF's frames as a viewer walks them: the count, the sum of their
/// delays in hundredths of a second, how many blocks ask it to loop, and
/// whether its one trailer closes the file. FFmpeg's decoder reads on past
/// a trailer; browsers stop there.
fn gif_blocks(gif: &[u8]) -> (usize, u32, usize, bool) {
    let sub_blocks = |mut at: usize| {
        while gif[at] != 0 {
            at += 1 + gif[at] as usize;
        }
        at + 1
    };
    let table = |flags: u8| {
        if flags & 0x80 != 0 {
            3 << ((flags & 7) + 1)
        } else {
            0
        }
    };
    let (mut frames, mut delays, mut loops) = (0, 0, 0);
    let mut at = 13 + table(gif[10]);
    while gif[at] != 0x3B {
        if gif[at] == 0x21 {
            if gif[at + 1] == 0xF9 {
                delays += u32::from(u16::from_le_bytes([gif[at + 4], gif[at + 5]]));
            }
            loops += usize::from(&gif[at + 3..at + 14] == b"NETSCAPE2.0");
            at = sub_blocks(at + 2);
        } else {
            assert_eq!(gif[at], 0x2C, "a block at byte {at}");
            frames += 1;
            at = sub_blocks(at + 11 + table(gif[at + 9]));
        }
    }
    (frames, delays, loops, at == gif.len() - 1)
}

#[test]
fn a_gif_is_a_silent_loop_at_its_size_and_rate_in_the_pictures_colours() {
    let fixture = Fixture::new();
    let quadrants = fixture.video(
        "quadrants.mp4",
        "nullsrc=s=320x180:r=30:d=1",
        "format=gbrp,geq=r='255*lt(X,W/2)':g='255*lt(Y,H/2)':b=64",
        Some(440),
    );
    // Six files: more than one batch decodes at once, so the GIF is joined
    // from parts mapped apart.
    let colours = [
        ("blue", BLUE),
        ("red", RED),
        ("green", GREEN),
        ("white", [255; 3]),
    ];
    let colours: Vec<(String, [u8; 3])> = colours
        .iter()
        .map(|(name, rgb)| (fixture.color(&format!("{name}.mp4"), name, None), *rgb))
        .collect();
    let settings = ExportSettings {
        kind: ExportKind::Gif,
        gif: GifSize::P240,
        ..ExportSettings::default()
    };
    // 15 fps, or the project's rate when it is lower.
    for (project, rate) in [(30.0, 15.0_f64), (10.0, 10.0)] {
        let mut timeline = Timeline::new(project).with_format(Format::Landscape);
        video(&mut timeline, &quadrants, 0, 0.0, 0.6, 0.0);
        for (index, (path, _)) in colours.iter().enumerate() {
            video(&mut timeline, path, 0, 0.6 + 0.2 * index as f64, 0.2, 0.0);
        }
        let name = format!("loop{project}.gif");
        let gif = fixture.export_with(&name, &timeline, &with(settings));
        let (streams, _) = contents(&gif);
        assert_eq!(streams, ["video:gif"], "{name}: no sound");
        let info = probe(gif.to_str().unwrap()).unwrap();
        assert_eq!((info.width, info.height), (428, 240), "{name}");
        let (count, delays, loops, closed) = gif_blocks(&std::fs::read(&gif).unwrap());
        let frames_in = (1.4 * rate).round() as usize;
        assert_eq!(count, frames_in, "{name}: 1.4 s at {rate} fps");
        assert_eq!(delays, 140, "{name}: its frames last 1.4 s");
        assert_eq!(
            (loops, closed),
            (1, true),
            "{name}: one loop block and trailer"
        );
        let pictures = frames(&gif);
        let points = [(107, 60), (321, 60), (107, 180), (321, 180)];
        let at = |time: f64| colors_at(&pictures[(time * rate) as usize], 428, &points);
        assert_colors(&name, &at(0.3), &[TL, TR, BL, BR]);
        for (index, (_, rgb)) in colours.iter().enumerate() {
            assert_colors(&name, &at(0.7 + 0.2 * index as f64), &[*rgb; 4]);
        }
    }
}

/// A mix: the clip before plays on past its out point and the next starts
/// before its in point, and each frame is the two mixed by how far through
/// it is. The cut is half a frame off the grid, as media cuts often are.
#[test]
fn a_mix_plays_both_clips_through_it_and_fades_by_how_far_through_it_is() {
    let fixture = Fixture::new();
    let ramp = |name, lum: &str, chroma: &str| {
        fixture.video(
            name,
            "nullsrc=s=64x36:r=30:d=2.5",
            &format!("geq=lum='{lum}':{chroma}"),
            None,
        )
    };
    // Far apart where they meet, and inside RGB: the test mixes RGB and
    // export mixes YUV, which agree only where no channel clips.
    let a = ramp("a.mp4", "20+2*N", "cb=128:cr=128");
    let b = ramp("b.mp4", "200-N", "cb=150:cr=110");
    let (a_frames, b_frames) = (frames(Path::new(&a)), frames(Path::new(&b)));
    let mut timeline = Timeline::new(30.0);
    let cut = 1.0 + 1.0 / 60.0;
    video(&mut timeline, &a, 0, 0.0, cut, 0.0);
    // Each file's source time is the timeline's: frame k of either is the
    // one timeline frame k shows.
    let next = video(&mut timeline, &b, 0, cut, 1.0, cut);
    timeline.clip_mut(next).unwrap().transition = Some(Transition::new(TransitionKind::Mix, 0.4));
    let exported = frames(&fixture.export("mix.mp4", &timeline));
    assert_eq!(exported.len(), 61);
    // From 0.2 s before the cut to 0.2 s after: frames 25 to 36. Each
    // frame's mean luma is compared: xfade truncates each mixed Y, Cb and
    // Cr, which on flat pictures puts blue up to 3 levels low but luma at
    // most 1.2. A picture a frame late is 10 levels off, half a frame 5.
    let luma = |frame: &[u8]| {
        frame
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
            .sum::<f64>()
            / (frame.len() / 3) as f64
    };
    for (k, frame) in exported.iter().enumerate() {
        let t = ((k as f64 / 30.0 - (cut - 0.2)) / 0.4).clamp(0.0, 1.0);
        let expected = luma(&a_frames[k]) * (1.0 - t) + luma(&b_frames[k]) * t;
        let bias = luma(frame) - expected;
        if std::env::var("PRINT").is_ok() {
            eprintln!("{k} {t:.3} {bias:.2}");
        }
        assert!(
            bias.abs() < 2.5,
            "frame {k} at {t:.3} of the mix: mean luma off by {bias:.2}"
        );
    }
}

/// Each kind halfway through, on quadrant media into flat blue: which half
/// of the canvas shows which picture, and which part of it. Clips are a
/// second each, so every transition into blue is centred on an even second;
/// the one at 8 s sits on the export's 8-second batch edge.
#[test]
fn every_transition_kind_draws_its_halfway_picture() {
    let fixture = Fixture::new();
    let quadrants = fixture.video(
        "quadrants.mp4",
        "nullsrc=s=320x180:r=30:d=2",
        "format=gbrp,geq=r='255*lt(X,W/2)':g='255*lt(Y,H/2)':b=64",
        None,
    );
    let blue = fixture.color("blue.mp4", "blue", None);
    const B: [u8; 3] = [0, 0, 255];
    const WHITE: [u8; 3] = [255, 255, 255];
    let half =
        |a: [u8; 3], b: [u8; 3]| [0, 1, 2].map(|i| ((u16::from(a[i]) + u16::from(b[i])) / 2) as u8);
    // Top left, bottom left, top right and bottom right of the canvas.
    let points = [(80, 45), (80, 135), (240, 45), (240, 135)];
    let kinds = [
        (
            TransitionKind::Mix,
            [half(TL, B), half(BL, B), half(TR, B), half(BR, B)],
        ),
        (TransitionKind::DipBlack, [BLACK; 4]),
        (TransitionKind::DipWhite, [WHITE; 4]),
        (TransitionKind::WipeLeft, [TL, BL, B, B]),
        (TransitionKind::WipeRight, [B, B, TR, BR]),
        (TransitionKind::WipeUp, [TL, B, TR, B]),
        (TransitionKind::WipeDown, [B, BL, B, BR]),
        // The quadrants have moved half a canvas left, up, right or down.
        (TransitionKind::SlideLeft, [TR, BR, B, B]),
        (TransitionKind::SlideRight, [B, B, TL, BL]),
        (TransitionKind::SlideUp, [BL, B, BR, B]),
        (TransitionKind::SlideDown, [B, TL, B, TR]),
    ];
    let mut timeline = Timeline::new(30.0);
    video(&mut timeline, &blue, 0, 0.0, 1.0, 0.0);
    for (index, (kind, _)) in kinds.iter().enumerate() {
        let at = 1.0 + 2.0 * index as f64;
        video(&mut timeline, &quadrants, 0, at, 1.0, 0.0);
        let into = video(&mut timeline, &blue, 0, at + 1.0, 1.0, 0.0);
        timeline.clip_mut(into).unwrap().transition = Some(Transition::new(*kind, 0.4));
    }
    let exported = frames(&fixture.export("kinds.mp4", &timeline));
    assert_eq!(exported.len(), 30 * (1 + 2 * kinds.len()));
    for (index, (kind, expected)) in kinds.iter().enumerate() {
        let cut = 30 * (2 + 2 * index);
        let at = |frame: usize| colors_at(&exported[frame], 320, &points);
        assert_colors(&format!("{kind:?} halfway"), &at(cut), expected);
        // It starts at 0.2 s before the cut and is gone 0.2 s after.
        assert_colors(&format!("{kind:?} before"), &at(cut - 7), &[TL, BL, TR, BR]);
        assert_colors(&format!("{kind:?} after"), &at(cut + 6), &[B; 4]);
    }
    // A dip is halfway to its colour a quarter and three quarters through.
    let cut = 30 * 4;
    let dimmed = |c: [u8; 3]| c.map(|v| v / 2);
    assert_colors(
        "dip a quarter through",
        &colors_at(&exported[cut - 3], 320, &points),
        &[TL, BL, TR, BR].map(dimmed),
    );
    assert_colors(
        "dip three quarters through",
        &colors_at(&exported[cut + 3], 320, &points),
        &[dimmed(B); 4],
    );
}

//! Public export regressions, checked by independently decoding every output
//! frame with FFmpeg. No native still/preview decoder is used as the oracle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::channel;

use openreel_engine::{ClipKind, Timeline, TitleStyle};
use openreel_media::{export_timeline, probe, ExportOptions};

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
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
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
                .chunks_exact(320 * 180 * 3)
                .map(<[u8]>::to_vec)
                .collect::<Vec<_>>()
        };
        let exported = accurate(&fixture.export(&format!("bt601-{range}-out.mp4"), &timeline));
        let original = accurate(Path::new(&source));
        assert_eq!(exported.len(), original.len());
        for (index, (actual, expected)) in exported.iter().zip(&original).enumerate() {
            let mut bias = [0.0_f64; 3];
            for (pixel, reference) in actual.chunks_exact(3).zip(expected.chunks_exact(3)) {
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

fn opacity_over_black(opacity: f32) {
    let fixture = Fixture::new();
    let red = fixture.color("red.mp4", "red", None);
    let blue = fixture.color("blue.mp4", "blue", None);
    let mut timeline = Timeline::new(10.0);
    let id = video(&mut timeline, &red, 0, 0.0, 0.3, 0.0);
    timeline.clip_mut(id).unwrap().opacity = opacity;
    video(&mut timeline, &blue, 1, 0.0, 0.3, 0.0);
    let expected: Vec<_> = frames(Path::new(&red))[0]
        .iter()
        .map(|&value| (value as f32 * opacity).round() as u8)
        .collect();
    let exported = fixture.export("opacity.mp4", &timeline);
    assert_frames(
        &format!("opacity={opacity} over black"),
        &frames(&exported),
        &[expected.as_slice(); 3],
    );
}

#[test]
fn zero_opacity_video_does_not_reveal_another_lane() {
    opacity_over_black(0.0);
}

#[test]
fn partial_opacity_video_fades_against_black() {
    opacity_over_black(0.5);
}

#[test]
fn occluded_video_resumes_at_advanced_source_time() {
    let fixture = Fixture::new();
    let source = fixture.indices("indices.mp4", "10");
    let original = frames(Path::new(&source));
    let black = vec![0; original[0].len()];
    let mut timeline = Timeline::new(10.0);
    video(&mut timeline, &source, 1, 0.0, 0.6, 0.0);
    timeline.add_clip("source-less picture", ClipKind::Video, 0, 0.2, 0.2, 0.0);
    let exported = fixture.export("occlusion.mp4", &timeline);
    assert_frames(
        "source-less winner and resumed source",
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
    let a = timeline.add_clip("FIRST WIDE TITLE", ClipKind::Title, 9, 0.0, 0.2, 0.0);
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
            .chunks_exact(3)
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
        .chunks_exact(3)
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
                fade_in: 1.0,
                fade_out: 0.0,
                duck: Vec::new(),
                clean_voice: false,
                smooth_fades: smooth,
            },
        )]);
        let mut preview = Vec::new();
        while preview.len() < 2 * 48_000 {
            let block = engine.render(1024);
            assert!(!block.is_empty(), "preview clock stopped");
            preview.extend(block.chunks_exact(2).map(|frame| frame[0]));
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
            fade_in: 0.0,
            fade_out: 0.0,
            duck: Vec::new().into(),
            clean_voice: true,
            smooth_fades: false,
        },
    )]);
    let mut preview = Vec::new();
    while preview.len() < 3 * 48_000 {
        let block = engine.render(1024);
        assert!(!block.is_empty(), "preview clock stopped");
        preview.extend(block.chunks_exact(2).map(|frame| frame[0]));
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
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()) as f64)
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
        speech: [(talk.clone(), vec![(1.0, 2.0)])].into(),
        ..Default::default()
    };
    let ducked = fixture.export_with("ducked.mp4", &timeline, &options);
    let before = tone_window(&ducked, 880.0, 0.1, 0.6);
    let during = tone_window(&ducked, 880.0, 1.35, 1.65);
    let after = tone_window(&ducked, 880.0, 2.4, 2.9);
    eprintln!("bed 880 Hz: before {before}, during {during}, after {after}");
    assert!(
        (0.2..0.32).contains(&(during / before)),
        "ducked to a quarter"
    );
    assert!((after / before - 1.0).abs() < 0.1, "recovers after speech");
    let voice = tone_window(&ducked, 440.0, 1.35, 1.65);
    assert!(
        (voice / tone_window(&ducked, 440.0, 0.1, 0.6) - 1.0).abs() < 0.1,
        "speech is untouched"
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

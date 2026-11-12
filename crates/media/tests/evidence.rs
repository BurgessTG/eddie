//! Public evidence APIs exercised with native decoding and real encoded media.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use openreel_media::{EvidenceFrame, EvidenceSampler, EvidenceVideo};

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "openreel-evidence-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let fixture = Self {
            path: dir.join("colors.mp4"),
            dir,
        };
        run(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-filter_threads",
                "1",
                "-f",
                "lavfi",
                "-i",
                "color=red:s=160x96:r=10:d=1[r];color=lime:s=160x96:r=10:d=1[g];\
                 color=blue:s=160x96:r=10:d=1[b];color=white:s=160x96:r=10:d=1[w];\
                 [r][g][b][w]concat=n=4:v=1:a=0",
                "-c:v",
                "libx264",
                "-threads:v",
                "1",
                "-g",
                "100",
                "-bf",
                "3",
                "-sc_threshold",
                "0",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&fixture.path));
        fixture
    }

    fn variant(&self, name: &str, input: &Path, options: &[&str]) -> PathBuf {
        let path = self.dir.join(name);
        run(Command::new("ffmpeg")
            .args(["-v", "error", "-threads", "1", "-filter_threads", "1", "-i"])
            .arg(input)
            .args(options)
            .arg(&path));
        path
    }

    fn detail_controls(&self) -> PathBuf {
        let raw_path = self.dir.join("detail.rgba");
        let mut rgba = Vec::new();
        for frame in 0..5 {
            for y in 0..96 {
                for x in 0..128 {
                    let grey = match frame {
                        0 => 128,
                        1 => 0,
                        2 => 255,
                        3 if x < 64 => {
                            if (x + y) % 2 == 0 {
                                64
                            } else {
                                192
                            }
                        }
                        3 if y < 48 => 200,
                        _ => 240,
                    };
                    let rgb = if frame == 4 { [255, 0, 0] } else { [grey; 3] };
                    rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
            }
        }
        std::fs::write(&raw_path, rgba).unwrap();
        let path = self.dir.join("detail.mkv");
        run(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-filter_threads",
                "1",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgba",
                "-video_size",
                "128x96",
                "-framerate",
                "1",
                "-i",
            ])
            .arg(raw_path)
            .args(["-c:v", "ffv1", "-threads:v", "1", "-pix_fmt", "bgr0"])
            .arg(&path));
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run(command: &mut Command) -> Output {
    let output = command.output().expect("run media tool");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn probe(path: &Path) -> serde_json::Value {
    let output = run(Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=best_effort_timestamp,best_effort_timestamp_time:stream=codec_name,width,height,time_base:format=start_time",
            "-of",
            "json",
        ])
        .arg(path));
    serde_json::from_slice(&output.stdout).unwrap()
}

fn timestamps(probe: &serde_json::Value) -> Vec<f64> {
    probe["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|frame| {
            frame["best_effort_timestamp_time"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect()
}

fn decode_rgba(path: &Path) -> Vec<u8> {
    run(Command::new("ffmpeg")
        .args(["-v", "error", "-threads", "1", "-filter_threads", "1", "-i"])
        .arg(path)
        .args([
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-threads:v",
            "1",
            "pipe:1",
        ]))
    .stdout
}

fn decode_jpegs(fixture: &Fixture, frames: &[EvidenceFrame]) -> Vec<u8> {
    let path = fixture.dir.join("frames.mjpg");
    let mut file = std::fs::File::create(&path).unwrap();
    for frame in frames {
        assert!(frame.jpeg.starts_with(&[0xff, 0xd8]));
        assert!(frame.jpeg.ends_with(&[0xff, 0xd9]));
        file.write_all(&frame.jpeg).unwrap();
    }
    drop(file);
    decode_rgba(&path)
}

fn assert_frames_match_stills(fixture: &Fixture, path: &Path, batches: &[&[f64]]) {
    let metadata = probe(path);
    let (numerator, denominator) = metadata["streams"][0]["time_base"]
        .as_str()
        .unwrap()
        .split_once('/')
        .unwrap();
    let numerator: f64 = numerator.parse().unwrap();
    let denominator: f64 = denominator.parse().unwrap();
    let origin: f64 = metadata["format"]["start_time"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    // Normalize the probed integer timestamps before converting to seconds,
    // so boundary requests do not inherit cancellation from a nonzero origin.
    let origin_ticks = (origin * denominator / numerator).round() as i64;
    let pts: Vec<f64> = metadata["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|frame| {
            (frame["best_effort_timestamp"].as_i64().unwrap() - origin_ticks) as f64 * numerator
                / denominator
        })
        .collect();
    let width = metadata["streams"][0]["width"].as_u64().unwrap() as u32;
    let height = metadata["streams"][0]["height"].as_u64().unwrap() as usize;
    let mut batch_sampler = EvidenceSampler::open(path.to_str().unwrap(), width).unwrap();
    let mut still_sampler = EvidenceSampler::open(path.to_str().unwrap(), width).unwrap();
    for requests in batches {
        let mut selected: Vec<(usize, f64)> = Vec::new();
        for request in *requests {
            let index = pts.iter().rposition(|time| time <= request).unwrap_or(0);
            if selected
                .last()
                .is_none_or(|(previous, _)| *previous != index)
            {
                selected.push((index, *request));
            }
        }
        let frames = batch_sampler.frames(requests).unwrap();
        let stills: Vec<_> = selected
            .iter()
            .map(|(_, request)| still_sampler.frame_at(*request).unwrap())
            .collect();
        assert_eq!(frames.len(), selected.len());
        assert!(frames.windows(2).all(|pair| pair[0].time < pair[1].time));
        let bytes: usize = frames.iter().map(|frame| frame.jpeg.len()).sum();
        assert!(bytes <= 16 * 1024 * 1024);
        for ((frame, still), (index, _)) in frames.iter().zip(&stills).zip(&selected) {
            assert_eq!(frame.time, pts[*index], "{}: {requests:?}", path.display());
            assert_eq!(frame.time, still.time);
        }
        let batch_pixels = decode_jpegs(fixture, &frames);
        let still_pixels = decode_jpegs(fixture, &stills);
        assert_eq!(
            batch_pixels.len(),
            frames.len() * width as usize * height * 4
        );
        assert!(
            batch_pixels == still_pixels,
            "{}: batch JPEG pixels differ from individual seeks for {requests:?}",
            path.display()
        );
        eprintln!(
            "{}: requests={requests:?}, batch/still/probed PTS={:?}, identical JPEG pixels={}, bytes={bytes}",
            path.file_name().unwrap().to_string_lossy(),
            frames.iter().map(|frame| frame.time).collect::<Vec<_>>(),
            batch_pixels.len() / 4,
        );
    }
    // Switching back from sequential decode must leave both still paths usable.
    assert_eq!(
        batch_sampler.frame_at(0.0).unwrap().jpeg,
        still_sampler.frame_at(0.0).unwrap().jpeg,
    );
    let batch_detail = batch_sampler
        .detail_at(2.0, [0.25, 0.25, 0.75, 0.75])
        .unwrap();
    let still_detail = still_sampler
        .detail_at(2.0, [0.25, 0.25, 0.75, 0.75])
        .unwrap();
    assert_eq!(batch_detail.frame.time, still_detail.frame.time);
    assert_eq!(batch_detail.frame.jpeg, still_detail.frame.jpeg);
}

fn mean_rgb(rgba: &[u8]) -> [f64; 3] {
    let mut sum = [0.0; 3];
    for pixel in rgba.as_chunks::<4>().0 {
        for channel in 0..3 {
            sum[channel] += pixel[channel] as f64;
        }
    }
    sum.map(|value| value / (rgba.len() / 4) as f64)
}

fn assert_video(fixture: &Fixture, video: &EvidenceVideo, expected: &[f64], fps: u32) {
    assert_eq!((video.width, video.height), (160, 96));
    assert_eq!(video.frame_times.len(), expected.len());
    assert!(video.frame_times.windows(2).all(|pair| pair[0] < pair[1]));
    for (actual, expected) in video.frame_times.iter().zip(expected) {
        assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
    }
    assert!(!video.mp4.is_empty() && video.mp4.len() <= 16 * 1024 * 1024);
    let path = fixture.dir.join("evidence.mp4");
    std::fs::write(&path, &video.mp4).unwrap();
    let metadata = probe(&path);
    assert_eq!(metadata["streams"][0]["codec_name"], "h264");
    assert_eq!(metadata["streams"][0]["width"], video.width);
    assert_eq!(metadata["streams"][0]["height"], video.height);
    let proxy_times = timestamps(&metadata);
    assert_eq!(proxy_times.len(), expected.len());
    for (index, time) in proxy_times.iter().enumerate() {
        assert!((time - index as f64 / fps as f64).abs() < 1e-6);
    }
    let rgba = decode_rgba(&path);
    let frame_bytes = 160 * 96 * 4;
    assert_eq!(rgba.len(), expected.len() * frame_bytes);
    let colors = [
        [255.0, 0.0, 0.0],
        [0.0, 255.0, 0.0],
        [0.0, 0.0, 255.0],
        [255.0; 3],
    ];
    for (pixels, source_time) in rgba.chunks_exact(frame_bytes).zip(expected) {
        let mean = mean_rgb(pixels);
        let color = colors[(source_time + 1e-6).floor() as usize];
        let distance: f64 = mean
            .iter()
            .zip(color)
            .map(|(actual, wanted)| (actual - wanted).abs())
            .sum();
        assert!(distance < 24.0, "PTS {source_time}: {mean:?} != {color:?}");
    }
    eprintln!(
        "source PTS={:?}, proxy PTS={proxy_times:?}, bytes={}",
        video.frame_times,
        video.mp4.len()
    );
}

#[test]
fn frames_match_stills_for_cfr_vfr_nonzero_origins_delayed_video_and_eof() {
    let fixture = Fixture::new();
    let offset = fixture.variant(
        "offset.mp4",
        &fixture.path,
        &["-c", "copy", "-output_ts_offset", "3.25"],
    );
    let vfr = fixture.variant(
        "vfr.mp4",
        &fixture.path,
        &[
            "-vf",
            "select='eq(n,0)+gte(n,20)'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-threads:v",
            "1",
            "-g",
            "100",
            "-bf",
            "3",
            "-sc_threshold",
            "0",
        ],
    );
    let vfr_offset = fixture.variant(
        "vfr-offset.mp4",
        &vfr,
        &["-c", "copy", "-output_ts_offset", "3.25"],
    );
    let delayed = fixture.variant(
        "delayed.mkv",
        &fixture.path,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=8000:cl=mono:d=5",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-vf",
            "setpts=PTS+1/TB",
            "-fps_mode",
            "passthrough",
            "-c:v",
            "ffv1",
            "-threads:v",
            "1",
            "-c:a",
            "pcm_s16le",
        ],
    );
    assert_eq!(timestamps(&probe(&offset))[0], 3.25);
    assert_eq!(&timestamps(&probe(&vfr))[..2], &[0.0, 2.0]);
    assert_eq!(&timestamps(&probe(&vfr_offset))[..2], &[3.25, 5.25]);
    let delayed_probe = probe(&delayed);
    assert_eq!(delayed_probe["format"]["start_time"], "0.000000");
    assert_eq!(timestamps(&delayed_probe)[0], 1.0);
    for path in [&fixture.path, &offset, &vfr, &vfr_offset, &delayed] {
        assert_frames_match_stills(
            &fixture,
            path,
            &[
                &[
                    0.0,
                    0.0,
                    0.01,
                    0.1 - 1e-10,
                    0.1,
                    0.1 + 1e-10,
                    0.99,
                    1.0,
                    1.05,
                    1.999999,
                    2.0,
                    2.0,
                    2.999999,
                    3.9,
                    4.0,
                    8.0,
                ],
                &[1.5, 1.9, 2.0],
                &[3.9, 4.9, 8.0],
                &[8.0, 8.0],
                &[0.0],
            ],
        );
    }
}

#[test]
fn frames_skip_zero_length_intervals_and_deduplicate_covering_requests() {
    let fixture = Fixture::new();
    let controls = fixture.detail_controls();
    let duplicate_pts = fixture.variant(
        "duplicate-pts.mkv",
        &controls,
        &[
            "-vf",
            "setpts=floor(N/2)/TB",
            "-fps_mode",
            "passthrough",
            "-c:v",
            "ffv1",
            "-threads:v",
            "1",
            "-pix_fmt",
            "bgr0",
        ],
    );
    assert_eq!(
        timestamps(&probe(&duplicate_pts)),
        vec![0.0, 0.0, 1.0, 1.0, 2.0]
    );
    assert_frames_match_stills(
        &fixture,
        &duplicate_pts,
        &[
            &[0.0, 0.0, 0.1, 0.999999, 1.0, 1.5, 1.999999, 2.0, 8.0],
            &[0.25, 0.75],
            &[1.0],
            &[8.0, 8.0],
        ],
    );
    let frames = EvidenceSampler::open(duplicate_pts.to_str().unwrap(), 128)
        .unwrap()
        .frames(&[0.0])
        .unwrap();
    assert!(
        mean_rgb(&decode_jpegs(&fixture, &frames))
            .iter()
            .all(|mean| *mean < 1.0),
        "the black second frame covers [0, 1), not the grey zero-length first frame"
    );
}

#[test]
fn frames_reject_invalid_inputs_and_accept_exact_count_and_span_limits() {
    let fixture = Fixture::new();
    let mut sampler = EvidenceSampler::open(fixture.path.to_str().unwrap(), 160).unwrap();
    for times in [
        vec![],
        vec![0.0; 41],
        vec![0.0, 30.000001],
        vec![1.0, 0.0],
        vec![-0.1],
        vec![f64::NAN],
        vec![f64::INFINITY],
        vec![f64::NEG_INFINITY],
        vec![0.0, f64::NAN, 1.0],
        vec![0.0, f64::INFINITY],
    ] {
        assert!(sampler.frames(&times).is_err(), "accepted {times:?}");
    }
    assert_eq!(sampler.frames(&[1.0; 40]).unwrap().len(), 1);
    let forty: Vec<f64> = (0..40).map(|index| index as f64 / 10.0).collect();
    let frames = sampler.frames(&forty).unwrap();
    assert_eq!(
        frames.iter().map(|frame| frame.time).collect::<Vec<_>>(),
        forty
    );
    for times in [[0.0, 30.0], [2.2, 32.2]] {
        let frames = sampler.frames(&times).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].time, times[0]);
        assert_eq!(frames[1].time, 3.9);
    }
    let long_gap = fixture.variant(
        "long-gap.mp4",
        &fixture.path,
        &[
            "-vf",
            "select='eq(n,0)+gte(n,20)',setpts='if(eq(N,0),0,PTS+29/TB)'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-threads:v",
            "1",
            "-g",
            "100",
            "-sc_threshold",
            "0",
        ],
    );
    assert_eq!(&timestamps(&probe(&long_gap))[..2], &[0.0, 31.0]);
    let mut sampler = EvidenceSampler::open(long_gap.to_str().unwrap(), 160).unwrap();
    let error = sampler.frames(&[30.9, 31.0]).unwrap_err();
    assert!(error.contains("30 source seconds"), "{error}");
    assert_eq!(sampler.frames(&[31.0]).unwrap()[0].time, 31.0);
    assert_eq!(sampler.frame_at(0.0).unwrap().time, 0.0);

    let exact_gap = fixture.variant(
        "exact-gap.mp4",
        &fixture.path,
        &[
            "-vf",
            "select='eq(n,0)+gte(n,20)',setpts='if(eq(N,0),0,PTS+28/TB)'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-threads:v",
            "1",
            "-g",
            "100",
            "-sc_threshold",
            "0",
        ],
    );
    assert_eq!(&timestamps(&probe(&exact_gap))[..2], &[0.0, 30.0]);
    assert_frames_match_stills(&fixture, &exact_gap, &[&[29.9, 30.0], &[0.0, 30.0]]);
}

#[test]
fn frames_reject_real_jpeg_payload_over_the_byte_limit_and_recover() {
    let fixture = Fixture::new();
    let path = fixture.dir.join("jpeg-noise.nut");
    run(Command::new("ffmpeg")
        .args([
            "-v", "error", "-filter_threads", "1", "-f", "lavfi", "-i",
            "nullsrc=s=1280x1280:r=2:d=20,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
            "-c:v", "ffv1", "-threads:v", "1", "-g", "1",
        ])
        .arg(&path));
    let times: Vec<f64> = (0..40).map(|index| index as f64 / 2.0).collect();
    let mut still_sampler = EvidenceSampler::open(path.to_str().unwrap(), 1280).unwrap();
    let mut bytes = 0;
    let mut prefix = 0;
    let mut last_bytes = 0;
    for time in &times {
        last_bytes = still_sampler.frame_at(*time).unwrap().jpeg.len();
        bytes += last_bytes;
        prefix += 1;
        if bytes > 16 * 1024 * 1024 {
            break;
        }
    }
    drop(still_sampler);
    assert!(
        bytes > 16 * 1024 * 1024,
        "fixture did not exceed the byte limit: {bytes}"
    );
    assert!(prefix > 1);
    let mut sampler = EvidenceSampler::open(path.to_str().unwrap(), 1280).unwrap();
    let allowed = sampler.frames(&times[..prefix - 1]).unwrap();
    assert_eq!(allowed.len(), prefix - 1);
    let allowed_bytes: usize = allowed.iter().map(|frame| frame.jpeg.len()).sum();
    assert_eq!(allowed_bytes, bytes - last_bytes);
    assert!(allowed_bytes <= 16 * 1024 * 1024);
    drop(allowed);
    drop(sampler);
    // Match the reference's fresh encoder state. JPEG rate control can shrink
    // a later batch enough to fit after encoding the accepted prefix above.
    let mut sampler = EvidenceSampler::open(path.to_str().unwrap(), 1280).unwrap();
    let error = sampler
        .frames(&times)
        .map(|frames| frames.iter().map(|frame| frame.jpeg.len()).sum::<usize>())
        .expect_err("accepted an oversized JPEG sequence (total bytes)");
    assert!(error.contains("16 MiB"), "{error}");
    assert_eq!(sampler.frames(&[0.0, 0.0]).unwrap().len(), 1);
    assert_eq!(sampler.frame_at(0.0).unwrap().time, 0.0);
    eprintln!(
        "real JPEG limit: {prefix} individually encoded frames total {bytes} bytes; \
         batch accepted {} frames / {allowed_bytes} bytes, rejected 40 requests: {error}",
        prefix - 1,
    );
}

#[test]
fn frames_work_without_external_tools() {
    if let Ok(path) = std::env::var("OPENREEL_EVIDENCE_FRAMES_SOURCE") {
        let mut sampler = EvidenceSampler::open(&path, 160).unwrap();
        let frames = sampler.frames(&[0.0, 0.99, 1.0, 2.0, 3.9, 8.0]).unwrap();
        assert_eq!(
            frames.iter().map(|frame| frame.time).collect::<Vec<_>>(),
            vec![0.0, 0.9, 1.0, 2.0, 3.9],
        );
        for frame in frames {
            assert!(frame.jpeg.starts_with(&[0xff, 0xd8]));
            assert!(frame.jpeg.ends_with(&[0xff, 0xd9]));
        }
        assert_eq!(sampler.frame_at(0.0).unwrap().time, 0.0);
        return;
    }
    let fixture = Fixture::new();
    let empty_tools = fixture.dir.join("empty-tools");
    std::fs::create_dir(&empty_tools).unwrap();
    let output = run(Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "frames_work_without_external_tools",
            "--nocapture",
        ])
        .env("OPENREEL_EVIDENCE_FRAMES_SOURCE", &fixture.path)
        .env("PATH", empty_tools));
    eprintln!(
        "empty-PATH native batch: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn frames_match_stills_for_repeated_dense_long_gop_windows() {
    let fixture = Fixture::new();
    let path = fixture.dir.join("dense.mp4");
    run(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-filter_threads",
            "1",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=30:d=12",
            "-c:v",
            "libx264",
            "-threads:v",
            "1",
            "-preset",
            "veryfast",
            "-crf",
            "18",
            "-g",
            "360",
            "-bf",
            "3",
            "-sc_threshold",
            "0",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path));
    let times: Vec<f64> = (260..300).map(|index| index as f64 / 30.0).collect();
    let mut batch_sampler = EvidenceSampler::open(path.to_str().unwrap(), 384).unwrap();
    let mut still_sampler = EvidenceSampler::open(path.to_str().unwrap(), 384).unwrap();
    // A second pass exercises decoder, buffer and JPEG encoder reuse.
    // Repeated performance measurements belong in the validation artifacts.
    for iteration in 0..2 {
        let batch = batch_sampler.frames(&times).unwrap();
        let stills: Vec<_> = times
            .iter()
            .map(|time| still_sampler.frame_at(*time).unwrap())
            .collect();
        assert_eq!(batch.len(), 40);
        assert_eq!(stills.len(), 40);
        for ((frame, still), time) in batch.iter().zip(&stills).zip(&times) {
            assert_eq!(frame.time, *time);
            assert_eq!(frame.time, still.time);
            assert!(
                frame.jpeg == still.jpeg,
                "dense JPEG mismatch at PTS {time}"
            );
        }
        if iteration == 0 {
            assert!(decode_jpegs(&fixture, &batch) == decode_jpegs(&fixture, &stills));
        }
    }
}

#[test]
fn video_preserves_covering_source_frames_for_cfr_vfr_and_nonzero_origins() {
    let fixture = Fixture::new();
    let offset = fixture.variant(
        "offset.mp4",
        &fixture.path,
        &["-c", "copy", "-output_ts_offset", "3.25"],
    );
    let vfr = fixture.variant(
        "vfr.mp4",
        &fixture.path,
        &[
            "-vf",
            "select='eq(n,0)+gte(n,20)'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-threads:v",
            "1",
            "-g",
            "100",
            "-bf",
            "3",
            "-sc_threshold",
            "0",
        ],
    );
    let vfr_offset = fixture.variant(
        "vfr-offset.mp4",
        &vfr,
        &["-c", "copy", "-output_ts_offset", "3.25"],
    );
    for (path, fps) in [
        (&fixture.path, 1),
        (&offset, 3),
        (&vfr, 11),
        (&vfr_offset, 30),
    ] {
        let metadata = probe(path);
        let origin: f64 = metadata["format"]["start_time"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let pts: Vec<f64> = timestamps(&metadata)
            .iter()
            .map(|time| time - origin)
            .collect();
        assert_eq!(
            pts.len(),
            if path == &vfr || path == &vfr_offset {
                21
            } else {
                40
            }
        );
        let mut sampler = EvidenceSampler::open(path.to_str().unwrap(), 160).unwrap();
        for requests in [
            &[
                0.0, 0.01, 0.99, 1.0, 1.05, 1.999999, 2.0, 2.0, 2.999999, 3.9, 4.0,
            ][..],
            &[1.5, 1.9, 2.0][..],
            &[3.9, 8.0][..],
            &[0.0][..],
        ] {
            let mut expected: Vec<f64> = requests
                .iter()
                .map(|requested| {
                    pts[pts
                        .iter()
                        .rposition(|time| *time <= requested + 1e-9)
                        .unwrap_or(0)]
                })
                .collect();
            expected.dedup();
            let video = sampler.video(requests, fps).unwrap();
            assert_video(&fixture, &video, &expected, fps);
        }
        for time in [0.99, 1.5, 2.0, 8.0] {
            let detail = sampler.detail_at(time, [0.25, 0.25, 0.75, 0.75]).unwrap();
            let expected = pts[pts.iter().rposition(|pts| *pts <= time + 1e-9).unwrap_or(0)];
            assert!((detail.frame.time - expected).abs() < 1e-6);
        }
        assert_eq!(sampler.frame_at(0.0).unwrap().time, 0.0);
    }
}

#[test]
fn video_returns_unique_pts_when_video_starts_after_audio_and_has_duplicate_timestamps() {
    let fixture = Fixture::new();
    let path = fixture.variant(
        "delayed-duplicates.mkv",
        &fixture.path,
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=8000:cl=mono:d=3",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-vf",
            "setpts=floor(N/2)/10/TB+1/TB",
            "-fps_mode",
            "passthrough",
            "-c:v",
            "ffv1",
            "-threads:v",
            "1",
            "-c:a",
            "pcm_s16le",
        ],
    );
    let metadata = probe(&path);
    let pts = timestamps(&metadata);
    assert_eq!(&pts[..4], &[1.0, 1.0, 1.1, 1.1]);
    assert_eq!(metadata["format"]["start_time"], "0.000000");
    let mut sampler = EvidenceSampler::open(path.to_str().unwrap(), 160).unwrap();
    let video = sampler.video(&[0.0, 1.0, 1.05, 1.1], 3).unwrap();
    assert_eq!(video.frame_times, vec![1.0, 1.1]);
    let output = fixture.dir.join("unique.mp4");
    std::fs::write(&output, video.mp4).unwrap();
    assert_eq!(timestamps(&probe(&output)).len(), 2);
    let rgba = decode_rgba(&output);
    assert_eq!(rgba.len(), 2 * 160 * 96 * 4);
    let mean = mean_rgb(&rgba);
    assert!(mean[0] > 240.0 && mean[1] < 10.0 && mean[2] < 10.0);
}

#[test]
fn video_rejects_invalid_inputs_and_accepts_exact_limits() {
    let fixture = Fixture::new();
    let mut sampler = EvidenceSampler::open(fixture.path.to_str().unwrap(), 160).unwrap();
    for times in [
        vec![],
        vec![0.0; 257],
        vec![0.0, 30.000001],
        vec![1.0, 0.0],
        vec![-0.1],
        vec![f64::NAN],
        vec![f64::INFINITY],
        vec![f64::NEG_INFINITY],
    ] {
        assert!(sampler.video(&times, 3).is_err(), "accepted {times:?}");
    }
    for fps in [0, 31, u32::MAX] {
        assert!(sampler.video(&[0.0], fps).is_err(), "accepted fps {fps}");
    }
    assert_video(
        &fixture,
        &sampler.video(&[1.0; 256], 30).unwrap(),
        &[1.0],
        30,
    );
    assert_video(
        &fixture,
        &sampler.video(&[0.0, 30.0], 1).unwrap(),
        &[0.0, 3.9],
        1,
    );
    assert_video(
        &fixture,
        &sampler.video(&[2.2, 32.2], 1).unwrap(),
        &[2.2, 3.9],
        1,
    );

    let long_gap = fixture.variant(
        "long-gap.mp4",
        &fixture.path,
        &[
            "-vf",
            "select='eq(n,0)+gte(n,20)',setpts='if(eq(N,0),0,PTS+29/TB)'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-threads:v",
            "1",
            "-g",
            "100",
            "-sc_threshold",
            "0",
        ],
    );
    let pts = timestamps(&probe(&long_gap));
    assert_eq!(&pts[..2], &[0.0, 31.0]);
    let mut sampler = EvidenceSampler::open(long_gap.to_str().unwrap(), 160).unwrap();
    assert!(
        sampler.video(&[30.9, 31.0], 3).is_err(),
        "actual PTS span exceeds 30 seconds"
    );
    assert_eq!(sampler.video(&[31.0], 3).unwrap().frame_times, vec![31.0]);
}

#[test]
fn video_rejects_real_high_entropy_output_over_the_byte_limit() {
    let fixture = Fixture::new();
    let path = fixture.dir.join("noise.nut");
    run(Command::new("ffmpeg")
        .args([
            "-v", "error", "-filter_threads", "1", "-f", "lavfi", "-i",
            "nullsrc=s=1280x720:r=30:d=3,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
            "-c:v", "ffv1", "-threads:v", "1",
        ])
        .arg(&path));
    let times: Vec<f64> = (0..90).map(|index| index as f64 / 30.0 + 0.0005).collect();
    let mut sampler = EvidenceSampler::open(path.to_str().unwrap(), 1280).unwrap();
    let small = sampler.video(&times[..3], 30).unwrap();
    assert_eq!((small.width, small.height), (1280, 720));
    assert_eq!(small.frame_times.len(), 3);
    let small_path = fixture.dir.join("small.mp4");
    std::fs::write(&small_path, small.mp4).unwrap();
    assert_eq!(timestamps(&probe(&small_path)).len(), 3);
    let result = sampler.video(&times, 30);
    assert!(
        result.is_err(),
        "accepted high-entropy output exceeding the byte limit"
    );
    eprintln!(
        "real high-entropy window rejected: {}",
        result.err().unwrap()
    );
    assert_eq!(sampler.frame_at(0.0).unwrap().time, 0.0);
}

#[test]
fn detail_measures_only_the_requested_rectangle_and_keeps_exact_crop_dimensions() {
    let fixture = Fixture::new();
    let path = fixture.detail_controls();
    let mut sampler = EvidenceSampler::open(path.to_str().unwrap(), 128).unwrap();
    for (time, luminance, clipped) in [
        (0.9, 128.0 / 255.0, 0.0),
        (1.0, 0.0, 1.0),
        (2.0, 1.0, 1.0),
        (4.0, 0.2126, 1.0),
    ] {
        let detail = sampler.detail_at(time, [0.0, 0.0, 1.0, 1.0]).unwrap();
        assert_eq!(detail.frame.time, time.floor());
        assert!((detail.luminance - luminance).abs() < 1e-6, "{detail:?}");
        assert_eq!(detail.clipped_fraction, clipped);
        assert!(
            detail.sharpness < 1e-10,
            "uniform crop has no spatial detail"
        );
    }
    let sharp = sampler.detail_at(3.0, [0.0, 0.0, 0.5, 1.0]).unwrap();
    let flat = sampler.detail_at(3.0, [0.5, 0.5, 1.0, 1.0]).unwrap();
    assert!((sharp.sharpness - (512.0_f32 / 255.0).powi(2)).abs() < 1e-5);
    assert!((sharp.luminance - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(sharp.clipped_fraction, 0.0);
    assert!(flat.sharpness < 1e-10);
    assert!((flat.luminance - 240.0 / 255.0).abs() < 1e-6);
    for (detail, expected_mean, height) in [(&sharp, 128.0, 96), (&flat, 240.0, 48)] {
        let path = fixture.dir.join("positioned-crop.jpg");
        std::fs::write(&path, &detail.frame.jpeg).unwrap();
        let rgba = decode_rgba(&path);
        assert_eq!(rgba.len(), 64 * height * 4);
        assert!(mean_rgb(&rgba)
            .iter()
            .all(|mean| (mean - expected_mean).abs() < 4.0));
    }
    let blurred_path = fixture.variant(
        "blurred.mkv",
        &path,
        &[
            "-vf",
            "gblur=sigma=2",
            "-c:v",
            "ffv1",
            "-threads:v",
            "1",
            "-pix_fmt",
            "bgr0",
        ],
    );
    let blurred = EvidenceSampler::open(blurred_path.to_str().unwrap(), 128)
        .unwrap()
        .detail_at(3.0, [0.0, 0.0, 0.5, 1.0])
        .unwrap();
    assert!(
        blurred.sharpness < sharp.sharpness,
        "blur must reduce measured detail"
    );
    eprintln!(
        "checkerboard variance={}, blurred variance={}, flat variance={}",
        sharp.sharpness, blurred.sharpness, flat.sharpness
    );

    for (bounds, dimensions) in [
        ([0.25, 0.25, 0.75, 0.75], (64, 48)),
        ([0.251, 0.251, 0.751, 0.751], (65, 49)),
        ([0.0, 0.0, 0.001, 0.001], (1, 1)),
    ] {
        let detail = sampler.detail_at(0.0, bounds).unwrap();
        let jpeg_path = fixture.dir.join("crop.jpg");
        std::fs::write(&jpeg_path, detail.frame.jpeg).unwrap();
        let metadata = probe(&jpeg_path);
        assert_eq!(metadata["streams"][0]["codec_name"], "mjpeg");
        assert_eq!(metadata["streams"][0]["width"], dimensions.0);
        assert_eq!(metadata["streams"][0]["height"], dimensions.1);
        let rgba = decode_rgba(&jpeg_path);
        assert_eq!(rgba.len(), dimensions.0 * dimensions.1 * 4);
        assert!(mean_rgb(&rgba)
            .iter()
            .all(|channel| (channel - 128.0).abs() < 4.0));
        assert!(detail.sharpness < 1e-10);
    }
    let whole = sampler.frame_at(0.0).unwrap();
    let path = fixture.dir.join("whole.jpg");
    std::fs::write(&path, whole.jpeg).unwrap();
    assert_eq!(probe(&path)["streams"][0]["width"], 128);
    assert_eq!(probe(&path)["streams"][0]["height"], 96);
}

#[test]
fn detail_rejects_malformed_regions_and_nonfinite_times() {
    let fixture = Fixture::new();
    let mut sampler = EvidenceSampler::open(fixture.path.to_str().unwrap(), 160).unwrap();
    for bounds in [
        [0.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0, 1.0],
        [0.7, 0.0, 0.2, 1.0],
        [0.0, 0.8, 1.0, 0.2],
        [-0.01, 0.0, 1.0, 1.0],
        [0.0, 0.0, 1.01, 1.0],
    ] {
        assert!(
            sampler.detail_at(0.0, bounds).is_err(),
            "accepted {bounds:?}"
        );
    }
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for index in 0..4 {
            let mut bounds = [0.0, 0.0, 1.0, 1.0];
            bounds[index] = invalid;
            assert!(sampler.detail_at(0.0, bounds).is_err());
        }
    }
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(sampler.detail_at(invalid, [0.0, 0.0, 1.0, 1.0]).is_err());
    }
    assert_eq!(
        sampler
            .detail_at(0.0, [0.0, 0.0, 1.0, 1.0])
            .unwrap()
            .frame
            .time,
        0.0
    );
}

#[cfg(unix)]
#[test]
fn video_reaps_encoder_and_removes_temporary_files_on_success_and_errors() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    if let Ok(mode) = std::env::var("OPENREEL_EVIDENCE_ENCODER_CASE") {
        let path = std::env::var("OPENREEL_EVIDENCE_SOURCE").unwrap();
        let mut sampler = EvidenceSampler::open(&path, 160).unwrap();
        let result = sampler.video(&[0.0, 1.0, 2.0, 3.0], 3);
        assert_eq!(result.is_ok(), mode == "success", "{mode}: {result:?}");
        if mode == "oversized" {
            assert!(result.unwrap_err().contains("16 MiB"));
        }
        assert_eq!(
            std::fs::read_dir(std::env::temp_dir()).unwrap().count(),
            0,
            "leaked temporary files"
        );
        let pid_path = std::env::var("OPENREEL_EVIDENCE_ENCODER_PID").unwrap();
        if mode != "spawn" {
            let pid = std::fs::read_to_string(pid_path).unwrap();
            let pid = pid.trim();
            // Without /proc, `ps -p` succeeds only for a process that exists.
            let exists = if cfg!(target_os = "linux") {
                Path::new("/proc").join(pid).exists()
            } else {
                Command::new("ps")
                    .args(["-p", pid])
                    .output()
                    .is_ok_and(|ps| ps.status.success())
            };
            assert!(!exists, "encoder not reaped");
        }
        assert_eq!(sampler.frame_at(0.0).unwrap().time, 0.0);
        return;
    }

    let fixture = Fixture::new();
    let path = std::env::var_os("PATH").unwrap();
    let real_ffmpeg = std::env::split_paths(&path)
        .map(|directory| directory.join("ffmpeg"))
        .find(|candidate| candidate.is_file())
        .unwrap();
    let tools = fixture.dir.join("tools");
    let empty_tools = fixture.dir.join("empty-tools");
    let temporary = fixture.dir.join("temporary");
    for directory in [&tools, &empty_tools, &temporary] {
        std::fs::create_dir(directory).unwrap();
    }
    let shim = tools.join("ffmpeg");
    std::fs::write(
        &shim,
        r#"#!/bin/sh
printf '%s\n' "$$" > "$OPENREEL_EVIDENCE_ENCODER_PID"
case "$OPENREEL_EVIDENCE_ENCODER_CASE" in
  success) exec "$OPENREEL_EVIDENCE_REAL_FFMPEG" "$@" ;;
  early-exit) head -c 131072 /dev/zero >&2; exit 23 ;;
  late-exit) cat >/dev/null; head -c 131072 /dev/zero >&2; exit 23 ;;
  oversized) cat >/dev/null; for output do :; done; head -c 16777217 /dev/zero > "$output" ;;
  empty) cat >/dev/null; for output do :; done; : > "$output" ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tool_path =
        std::env::join_paths(std::iter::once(tools).chain(std::env::split_paths(&path))).unwrap();
    for mode in [
        "success",
        "early-exit",
        "late-exit",
        "oversized",
        "empty",
        "spawn",
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "video_reaps_encoder_and_removes_temporary_files_on_success_and_errors",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OPENREEL_EVIDENCE_ENCODER_CASE", mode)
            .env("OPENREEL_EVIDENCE_SOURCE", &fixture.path)
            .env(
                "OPENREEL_EVIDENCE_ENCODER_PID",
                fixture.dir.join("encoder.pid"),
            )
            .env("OPENREEL_EVIDENCE_REAL_FFMPEG", &real_ffmpeg)
            // The shim is found through PATH; macOS would otherwise go
            // looking for an FFmpeg that draws text.
            .env("EDDIE_FFMPEG", "ffmpeg")
            .env("TMPDIR", &temporary)
            .env(
                "PATH",
                if mode == "spawn" {
                    empty_tools.as_os_str()
                } else {
                    tool_path.as_os_str()
                },
            )
            .spawn()
            .unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("evidence encoder cleanup stalled for {mode}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "cleanup case {mode}");
        assert_eq!(std::fs::read_dir(&temporary).unwrap().count(), 0, "{mode}");
    }
}

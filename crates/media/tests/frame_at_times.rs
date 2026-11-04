//! Content and timing regressions against real libavcodec decoding.
//! Frames cover [PTS, next PTS); stills clamp to the last frame at EOF.
//! The long-GOP source has red/lime/blue/white one-second sections.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use openreel_media::{frame_at, scaled_height, MediaResult, RawFrame};

// Compile the actual private native implementation, rather than the public
// frame_at wrapper (which can hide native failures with a CLI fallback).
// This keeps VideoDecoder out of the production public API.
#[allow(dead_code)]
#[path = "../src/decode.rs"]
mod native;

const COLORS: [(&str, (f64, f64, f64)); 4] = [
    ("red", (255.0, 0.0, 0.0)),
    ("lime", (0.0, 255.0, 0.0)),
    ("blue", (0.0, 0.0, 255.0)),
    ("white", (255.0, 255.0, 255.0)),
];

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn vfr(&self) -> PathBuf {
        let path = self.dir.join("vfr.mp4");
        run_ffmpeg(
            Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-i"])
                .arg(&self.path)
                .args([
                    "-vf",
                    "select='eq(n,0)+gte(n,20)'",
                    "-fps_mode",
                    "vfr",
                    "-c:v",
                    "libx264",
                    "-g",
                    "100",
                    "-sc_threshold",
                    "0",
                    "-bf",
                    "3",
                    "-threads:v",
                    "1",
                ])
                .arg(&path),
        );
        let pts = presentation_times(&path);
        assert_eq!(pts.len(), 21);
        assert_eq!(&pts[..3], &[0.0, 2.0, 2.1]);
        assert_eq!(pts.last(), Some(&3.9));
        path
    }

    fn nonzero_origin(&self) -> PathBuf {
        let path = self.dir.join("offset.mp4");
        run_ffmpeg(
            Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-i"])
                .arg(&self.path)
                .args(["-c", "copy", "-output_ts_offset", "3.25"])
                .arg(&path),
        );
        let pts = presentation_times(&path);
        assert_eq!(pts.len(), 40);
        assert_eq!(pts[0], 3.25);
        assert_eq!(pts[10], 4.25);
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_ffmpeg(cmd: &mut Command) {
    let out = cmd.output().expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn presentation_times(path: &Path) -> Vec<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=best_effort_timestamp_time",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .expect("ffprobe frame timestamps");
    assert!(out.status.success(), "{:?}", out.stderr);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("frame JSON");
    let pts: Vec<f64> = json["frames"]
        .as_array()
        .expect("frames")
        .iter()
        .map(|frame| {
            frame["best_effort_timestamp_time"]
                .as_str()
                .expect("frame PTS")
                .parse()
                .expect("numeric PTS")
        })
        .collect();
    eprintln!("{}: source PTS={pts:?}", path.display());
    pts
}

fn make_color_segments_video() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "openreel_frame_at_{}_{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).expect("temp dir");
    let fixture = Fixture {
        path: dir.join("segments.mp4"),
        dir,
    };
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error"]);
    for (name, _) in COLORS {
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={name}:s=320x180:r=10:d=1"),
        ]);
    }
    let inputs: String = (0..COLORS.len()).map(|i| format!("[{i}:v]")).collect();
    cmd.args([
        "-filter_complex",
        &format!("{inputs}concat=n={}:v=1:a=0[v]", COLORS.len()),
        "-map",
        "[v]",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-g",
        "100",
        "-sc_threshold",
        "0",
        "-bf",
        "3",
        "-threads:v",
        "1",
        "-pix_fmt",
        "yuv420p",
    ])
    .arg(&fixture.path);
    run_ffmpeg(&mut cmd);
    fixture
}

fn mean_rgb(rgba: &[u8]) -> (f64, f64, f64) {
    let pixels = (rgba.len() / 4) as f64;
    let (mut r, mut g, mut b) = (0.0_f64, 0.0_f64, 0.0_f64);
    for px in rgba.chunks_exact(4) {
        r += px[0] as f64;
        g += px[1] as f64;
        b += px[2] as f64;
    }
    (r / pixels, g / pixels, b / pixels)
}

fn color_distance(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs() + (a.2 - b.2).abs()
}

fn assert_color(mean: (f64, f64, f64), color: usize, context: &str) {
    let (name, expected) = COLORS[color];
    eprintln!("{context}: RGB={mean:?}, expected {name}");
    assert!(
        color_distance(mean, expected) < 24.0,
        "{context}: expected {name} near {expected:?}, got {mean:?}"
    );
}

fn frame_mean(frame: &RawFrame) -> (f64, f64, f64) {
    assert_eq!((frame.width, frame.height), (320, 180));
    assert_eq!(frame.rgba.len(), 320 * 180 * 4);
    mean_rgb(&frame.rgba)
}

#[test]
fn color_oracle_rejects_black_ties() {
    for (_, expected) in COLORS {
        assert!(color_distance((0.0, 0.0, 0.0), expected) >= 24.0);
    }
}

#[test]
fn visual_model_evidence_is_valid_jpeg_with_actual_source_pts() {
    let fixture = make_color_segments_video();
    for (path, vfr) in [
        (fixture.path.clone(), false),
        (fixture.nonzero_origin(), false),
        (fixture.vfr(), true),
    ] {
        let mut sampler = openreel_media::EvidenceSampler::open(path.to_str().unwrap(), 320)
            .expect("native evidence sampler");
        for (requested, pts, color) in [
            (0.99, if vfr { 0.0 } else { 0.9 }, 0),
            (1.0, if vfr { 0.0 } else { 1.0 }, if vfr { 0 } else { 1 }),
            (2.0, 2.0, 2),
            (3.9, 3.9, 3),
            (8.0, 3.9, 3),
            (0.0, 0.0, 0),
        ] {
            let evidence = sampler.frame_at(requested).expect("JPEG evidence");
            assert!(
                (evidence.time - pts).abs() < 1e-9,
                "requested {requested}, actual {}",
                evidence.time
            );
            let jpeg_path = fixture.dir.join("evidence.jpg");
            std::fs::write(&jpeg_path, evidence.jpeg).expect("write evidence");
            let decoded = Command::new("ffmpeg")
                .args(["-v", "error", "-threads", "1", "-i"])
                .arg(&jpeg_path)
                .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .output()
                .expect("independently decode JPEG");
            assert!(decoded.status.success(), "{:?}", decoded.stderr);
            assert_eq!(decoded.stdout.len(), 320 * 180 * 4);
            assert_color(mean_rgb(&decoded.stdout), color, "model evidence");
        }
        assert!(sampler.frame_at(f64::NAN).is_err());
        assert!(sampler.frame_at(f64::INFINITY).is_err());
        assert_eq!(sampler.frame_at(0.0).unwrap().time, 0.0);
    }
}

#[test]
fn native_video_decoder_selects_half_open_boundaries() {
    let fixture = make_color_segments_video();
    let mut decoder =
        native::VideoDecoder::open(fixture.path.to_str().unwrap(), 320, false).unwrap();
    for (time, color) in [
        (0.0, 0),
        (0.5, 0),
        (0.999999, 0),
        (1.0, 1),
        (1.000001, 1),
        (1.999999, 1),
        (2.0, 2),
        (2.999999, 2),
        (3.0, 3),
        (0.5, 0),
    ] {
        let frame = decoder.frame_at(time).expect("native frame");
        assert_color(frame_mean(&frame), color, &format!("native t={time}"));
    }
}

#[test]
fn native_video_decoder_drains_final_gop_and_reseeks() {
    let fixture = make_color_segments_video();
    let mut decoder =
        native::VideoDecoder::open(fixture.path.to_str().unwrap(), 320, true).unwrap();
    for (time, color) in [
        (3.5, 3),
        (3.9, 3),
        (3.999999, 3),
        (4.0, 3),
        (4.5, 3),
        (0.5, 0),
    ] {
        let frame = decoder.frame_at(time).expect("native tail frame");
        assert_color(frame_mean(&frame), color, &format!("native tail t={time}"));
    }
}

#[test]
fn native_video_decoder_streams_all_frames_and_reuses_buffer() {
    let fixture = make_color_segments_video();
    let mut decoder =
        native::VideoDecoder::open(fixture.path.to_str().unwrap(), 320, true).unwrap();
    let mut buffer = vec![0; 320 * 180 * 4];
    let allocation = buffer.as_ptr();
    for index in 0..40 {
        let (pts, frame) = decoder.next_frame(buffer).expect("next native frame");
        assert!(
            (pts - index as f64 / 10.0).abs() < 1e-9,
            "frame {index}: {pts}"
        );
        assert_color(
            frame_mean(&frame),
            index / 10,
            &format!("sequential PTS={pts}"),
        );
        assert_eq!(
            frame.rgba.as_ptr(),
            allocation,
            "reuse caller's RGBA storage"
        );
        buffer = frame.rgba;
    }
    assert!(decoder.next_frame(buffer).is_none(), "actual decoder EOF");
    assert!(
        decoder.next_frame(Vec::new()).is_none(),
        "EOF remains drained"
    );
}

#[test]
fn native_video_decoder_uses_vfr_presentation_intervals() {
    let fixture = make_color_segments_video();
    let path = fixture.vfr();
    let mut decoder = native::VideoDecoder::open(path.to_str().unwrap(), 320, false).unwrap();
    for (time, color) in [
        (1.0, 0),
        (1.5, 0),
        (1.999999, 0),
        (2.0, 2),
        (3.0, 3),
        (3.9, 3),
    ] {
        let frame = decoder.frame_at(time).expect("native VFR frame");
        assert_color(frame_mean(&frame), color, &format!("VFR t={time}"));
    }
}

#[test]
fn native_video_decoder_normalizes_nonzero_origin() {
    let fixture = make_color_segments_video();
    let path = fixture.nonzero_origin();
    let mut decoder = native::VideoDecoder::open(path.to_str().unwrap(), 320, false).unwrap();
    for (time, color) in [(1.0, 1), (1.5, 1), (3.5, 3), (0.0, 0)] {
        let frame = decoder.frame_at(time).expect("native offset frame");
        assert_color(frame_mean(&frame), color, &format!("offset t={time}"));
    }
    decoder.seek(0.0).expect("seek origin");
    let (pts, frame) = decoder.next_frame(Vec::new()).expect("first source frame");
    assert!(pts.abs() < 1e-9, "normalized first PTS: {pts}");
    assert_color(frame_mean(&frame), 0, "normalized first frame");
}

#[test]
fn still_decoder_returns_frame_at_requested_time() {
    let fixture = make_color_segments_video();
    let path_str = fixture.path.to_str().expect("utf8 path").to_owned();
    let (decoder, rx) = openreel_media::StillDecoder::spawn(|| {});
    for (i, (t, color)) in [(0.5, 0), (1.0, 1), (1.5, 1), (2.5, 2), (3.5, 3), (0.5, 0)]
        .into_iter()
        .enumerate()
    {
        decoder.request(i as u64 + 1, path_str.clone(), 320, 180, t, 320, false);
        let (id, result) = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("still result");
        assert_eq!(id, i as u64 + 1);
        let frame = result.expect("decode ok");
        assert_color(frame_mean(&frame), color, &format!("StillDecoder t={t}"));
    }
    drop(decoder);
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)),
        Err(RecvTimeoutError::Disconnected)
    ));
}

/// Preview converts with each source's own matrix and range, as export
/// does, so BT.709 HD footage is not read as BT.601 (greens and reds came
/// out about 20 levels too strong). Untagged sources match the FFmpeg CLI.
#[test]
fn native_decoder_uses_the_source_color_matrix_and_range() {
    let dir = std::env::temp_dir().join(format!(
        "openreel_color_{}_{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).expect("temp dir");
    let _cleanup = Fixture {
        path: dir.join("unused"),
        dir: dir.clone(),
    };
    let source = [32_u8, 176, 64];
    let center = |rgba: &[u8], width: usize, height: usize| {
        let i = ((height / 2) * width + width / 2) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    // (name, encode matrix, encode range, tags), all from the same RGB.
    let cases = [
        ("bt709-tv", "bt709", "tv", Some(("bt709", "tv"))),
        ("bt601-tv", "bt601", "tv", Some(("smpte170m", "tv"))),
        ("bt709-pc", "bt709", "pc", Some(("bt709", "pc"))),
        ("untagged", "bt709", "tv", None),
    ];
    for (name, matrix, range, tags) in cases {
        let path = dir.join(format!("{name}.mp4"));
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg("color=c=0x20B040:s=64x36:r=10:d=0.5,format=gbrp")
            .arg("-vf")
            .arg(format!(
                "scale=out_color_matrix={matrix}:out_range={range},format=yuv420p"
            ))
            .args(["-c:v", "libx264", "-qp", "0"]);
        if let Some((space, range)) = tags {
            cmd.args(["-colorspace", space, "-color_range", range]);
        }
        run_ffmpeg(cmd.arg(&path));
        let expected = match tags {
            Some(_) => source,
            None => {
                let out = Command::new("ffmpeg")
                    .args(["-v", "error", "-i"])
                    .arg(&path)
                    .args([
                        "-frames:v",
                        "1",
                        "-vf",
                        "format=rgb24",
                        "-f",
                        "rawvideo",
                        "-",
                    ])
                    .output()
                    .expect("ffmpeg reference decode");
                let i = (18 * 64 + 32) * 3;
                [out.stdout[i], out.stdout[i + 1], out.stdout[i + 2]]
            }
        };
        for fast in [false, true] {
            let frame = native::VideoDecoder::open(path.to_str().unwrap(), 64, fast)
                .and_then(|mut decoder| decoder.frame_at(0.0))
                .expect("decode color patch");
            let got = center(&frame.rgba, frame.width as usize, frame.height as usize);
            let off = (0..3)
                .map(|c| (got[c] as i32 - expected[c] as i32).abs())
                .max()
                .unwrap();
            assert!(
                off <= 3,
                "{name} fast={fast}: got {got:?}, expected {expected:?}"
            );
        }
    }
}

/// A contact sheet shows each cell's source picture at its time, darkens
/// faded pictures, leaves pictureless cells black, and draws titles over
/// the picture as export places them.
#[test]
fn contact_sheet_shows_each_cells_picture_opacity_and_title() {
    use openreel_media::{contact_sheet, SheetCell};
    let fixture = make_color_segments_video();
    let path = fixture.path.to_str().unwrap().to_owned();
    let cell = |picture: Option<f64>, opacity: f32, title: Option<&str>| SheetCell {
        picture: picture.map(|time| (path.clone(), time)),
        opacity,
        title: title.map(|text| (text.to_owned(), openreel_engine::TitleStyle::Caption, 1.0)),
        label: "cell".to_owned(),
    };
    let cells = [
        cell(Some(2.5), 1.0, None),         // blue, before red in source
        cell(Some(0.5), 1.0, None),         // red
        cell(Some(1.5), 0.5, None),         // lime at half opacity
        cell(None, 1.0, None),              // gap
        cell(Some(3.5), 1.0, Some("WWWW")), // white under a caption
    ];
    let jpeg = contact_sheet(&cells, (1920, 1080)).expect("contact sheet");
    let sheet = fixture.dir.join("sheet.jpg");
    std::fs::write(&sheet, &jpeg).unwrap();
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&sheet)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .expect("decode sheet");
    // Four 384x216 cells per row, a 4 px gutter and a 26 px label strip.
    let (width, cell_w, cell_h) = (4 * 384 + 5 * 4, 384, 216);
    assert_eq!(out.stdout.len(), width * (2 * (cell_h + 26) + 3 * 4) * 3);
    let at = |index: usize, fx: f64, fy: f64| {
        let x = 4 + (index % 4) * (cell_w + 4) + (fx * cell_w as f64) as usize;
        let y = 4 + (index / 4) * (cell_h + 26 + 4) + (fy * cell_h as f64) as usize;
        let i = (y * width + x) * 3;
        (
            out.stdout[i] as f64,
            out.stdout[i + 1] as f64,
            out.stdout[i + 2] as f64,
        )
    };
    assert_color(at(0, 0.5, 0.3), 2, "cell 0");
    assert_color(at(1, 0.5, 0.3), 0, "cell 1");
    let faded = at(2, 0.5, 0.3);
    assert!(
        (100.0..156.0).contains(&faded.1) && faded.0 < 24.0,
        "half-opacity lime: {faded:?}"
    );
    let gap = at(3, 0.5, 0.3);
    assert!(gap.0 + gap.1 + gap.2 < 24.0, "gap cell: {gap:?}");
    assert_color(at(4, 0.5, 0.3), 3, "cell 4 picture");
    // The caption's dark box sits near the bottom centre of a white picture.
    let caption = (0..cell_w)
        .map(|x| at(4, x as f64 / cell_w as f64, 1.0 - 90.0 / 1080.0 + 0.02))
        .filter(|rgb| rgb.0 < 160.0)
        .count();
    assert!(caption > 20, "no caption box drawn ({caption} dark pixels)");
}

/// Scrubbing reuses one decoder: forward steps decode on, backward steps
/// read recent pictures, other moves seek, and the width can change between
/// requests. Every answer must equal a fresh seek-and-decode of that time.
#[test]
fn still_decoder_scrub_sequence_matches_fresh_decodes() {
    let dir = std::env::temp_dir().join(format!(
        "openreel_scrub_{}_{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).expect("temp dir");
    let fixture = Fixture {
        path: dir.join("ramp.mp4"),
        dir,
    };
    // Every frame has its own brightness; eight-frame GOPs with B-frames.
    run_ffmpeg(
        Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg("color=black:s=64x36:r=10:d=4,geq=lum='16+N*5':cb=128:cr=128")
            .args(["-c:v", "libx264", "-qp", "0", "-g", "8", "-bf", "3"])
            .args([
                "-sc_threshold",
                "0",
                "-threads:v",
                "1",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&fixture.path),
    );
    let path = fixture.path.to_str().unwrap().to_owned();
    let fresh = |time: f64, width: u32| {
        native::VideoDecoder::open(&path, width, true)
            .and_then(|mut decoder| decoder.frame_at(time))
            .expect("fresh decode")
    };
    for index in 0..39 {
        let (a, b) = (index as f64 / 10.0 + 0.05, (index + 1) as f64 / 10.0 + 0.05);
        assert_ne!(
            fresh(a, 64).rgba,
            fresh(b, 64).rgba,
            "frames {index} and next"
        );
    }

    let (decoder, rx) = openreel_media::StillDecoder::spawn(|| {});
    // Seconds; 10 fps, keyframes every 0.8 s. Mid-frame times and exact
    // frame starts (which belong to the starting frame).
    let requests: &[(f64, u32)] = &[
        (0.55, 64),
        (0.65, 64),
        (0.7, 64),  // exact start of the next frame
        (0.85, 64), // forward across a keyframe
        (0.95, 64),
        (1.0, 64),
        (0.9, 64), // back through recent pictures, exact start
        (0.75, 64),
        (0.65, 64),
        (0.65, 32), // narrower output, same position
        (0.55, 32),
        (0.35, 32), // further back than recent pictures
        (0.45, 64),
        (2.05, 64), // forward past several keyframes
        (1.95, 64),
        (2.15, 64),
        (3.95, 64),
        (6.0, 64), // past the end clamps to the last picture
        (3.85, 64),
        (0.05, 64),
        (-0.5, 64), // before the start clamps to the first picture
        (0.15, 64),
    ];
    for (id, &(time, width)) in requests.iter().enumerate() {
        decoder.request(id as u64, path.clone(), 64, 36, time, width, true);
        let (answered, result) = rx.recv_timeout(Duration::from_secs(30)).expect("still");
        assert_eq!(answered, id as u64);
        let still = result.expect("still decode");
        let expected = fresh(time, width);
        assert_eq!(
            (still.width, still.height),
            (expected.width, expected.height)
        );
        assert!(
            still.rgba == expected.rgba,
            "request {id}: {time} s at {width}px"
        );
    }
}

fn collect_stream(path: &Path, start: f64) -> Vec<(f64, (f64, f64, f64))> {
    let stream =
        openreel_media::FrameStream::spawn(path.to_str().unwrap(), 320, 180, start, 320, 30.0)
            .expect("native FrameStream");
    let mut first_received = None;
    let mut observed = Vec::new();
    loop {
        match stream.frames.recv_timeout(Duration::from_secs(5)) {
            Ok(frame) => {
                let elapsed = first_received
                    .get_or_insert_with(Instant::now)
                    .elapsed()
                    .as_secs_f64();
                observed.push((elapsed, frame_mean(&frame)));
                stream.recycle(frame.rgba);
                assert!(observed.len() <= 40, "stream must terminate at source EOF");
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(error) => panic!("stream did not finish: {error}"),
        }
    }
    stream.stop();
    eprintln!("FrameStream start={start}: (elapsed seconds, RGB)={observed:?}");
    observed
}

fn assert_cfr_in_point(path: &Path) {
    let observed = collect_stream(path, 1.5);
    assert_color(observed[0].1, 1, "stream first at source 1.5");
    assert_eq!(observed.len(), 25, "only source frames 1.5 through 3.9");
    for (index, (_, mean)) in observed.iter().enumerate() {
        assert_color(*mean, (15 + index) / 10, &format!("stream frame {index}"));
    }
    assert!(
        (0.30..1.20).contains(&observed[5].0),
        "source 2.0 is due 0.5 seconds after in-point, got {}",
        observed[5].0
    );
}

#[test]
fn frame_stream_discards_preroll_and_reaches_final_frame() {
    let fixture = make_color_segments_video();
    assert_cfr_in_point(&fixture.path);
}

#[test]
fn frame_stream_paces_from_inside_a_vfr_interval() {
    let fixture = make_color_segments_video();
    let path = fixture.vfr();
    let observed = collect_stream(&path, 1.5);
    assert_eq!(observed.len(), 21);
    assert_color(observed[0].1, 0, "VFR frame covering source 1.5");
    for (index, (_, mean)) in observed.iter().enumerate().skip(1) {
        assert_color(*mean, if index <= 10 { 2 } else { 3 }, "VFR stream");
    }
    assert!(
        (0.30..1.20).contains(&observed[1].0),
        "source 2.0 is due after the remaining 0.5 seconds, not the full 2-second interval: {}",
        observed[1].0
    );
}

#[test]
fn frame_stream_normalizes_nonzero_origin() {
    let fixture = make_color_segments_video();
    let path = fixture.nonzero_origin();
    assert_cfr_in_point(&path);
}

#[test]
fn frame_stream_stops_under_backpressure() {
    let fixture = make_color_segments_video();
    let stream = openreel_media::FrameStream::spawn(
        fixture.path.to_str().unwrap(),
        320,
        180,
        0.0,
        320,
        30.0,
    )
    .unwrap();
    // Leave the three-frame output queue full while the producer waits.
    std::thread::sleep(Duration::from_millis(600));
    let stopping = Instant::now();
    stream.stop();
    eprintln!("stop with full queue: {:?}", stopping.elapsed());
    assert!(stopping.elapsed() < Duration::from_secs(1));
}

fn await_preview(
    preview: &openreel_media::FrameStream,
    generation: u64,
    time: f64,
) -> MediaResult<RawFrame> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = preview.preview_frame(generation, time) {
            return result;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "preview generation {generation} timed out at {time}"
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn preview_follows_the_callers_clock_not_decoder_startup_or_consumer_delay() {
    let fixture = make_color_segments_video();
    let preview = openreel_media::FrameStream::spawn_preview(|| {}).unwrap();
    let generation = preview
        .request_preview(fixture.path.to_str().unwrap(), 0.5, 320, 30.0)
        .unwrap();
    let frame = await_preview(&preview, generation, 0.5).unwrap();
    assert_color(frame_mean(&frame), 0, "preview at source 0.5");
    preview.recycle(frame.rgba);

    std::thread::sleep(Duration::from_millis(650));
    assert!(
        preview.preview_frame(generation, 0.5).is_none(),
        "wall time cannot advance the caller's clock"
    );
    // A delayed UI advances by two seconds. There must be no queue of old
    // red/green pictures to drain before the current blue interval appears.
    let frame = await_preview(&preview, generation, 2.5).unwrap();
    assert_color(frame_mean(&frame), 2, "preview catches up directly to 2.5");
    preview.recycle(frame.rgba);
    let frame = await_preview(&preview, generation, 3.9).unwrap();
    assert_color(frame_mean(&frame), 3, "preview final delayed frame");
    preview.recycle(frame.rgba);
    assert!(
        preview.preview_frame(generation, 8.0).is_none(),
        "EOF holds the already displayed final picture"
    );

    let next = preview
        .request_preview(fixture.path.to_str().unwrap(), 0.0, 320, 30.0)
        .unwrap();
    assert!(
        preview.preview_frame(generation, 3.9).is_none(),
        "old consumers cannot advance the new generation"
    );
    let frame = await_preview(&preview, next, 0.0).unwrap();
    assert_color(frame_mean(&frame), 0, "preview reseeks after EOF");
    preview.recycle(frame.rgba);
    preview.stop();
}

#[test]
fn preview_preserves_vfr_intervals_nonzero_origins_and_eof_clamping() {
    let fixture = make_color_segments_video();
    let preview = openreel_media::FrameStream::spawn_preview(|| {}).unwrap();
    for (path, first_color) in [(fixture.vfr(), 0), (fixture.nonzero_origin(), 1)] {
        for start in [1.5, 1.999999] {
            let generation = preview
                .request_preview(path.to_str().unwrap(), start, 320, 30.0)
                .unwrap();
            let first = await_preview(&preview, generation, start).unwrap();
            assert_color(
                frame_mean(&first),
                first_color,
                &format!("preview source interval containing {start}"),
            );
            preview.recycle(first.rgba);
            let frame = await_preview(&preview, generation, 2.0).unwrap();
            assert_color(frame_mean(&frame), 2, "preview half-open boundary at 2.0");
            preview.recycle(frame.rgba);
        }

        let generation = preview
            .request_preview(path.to_str().unwrap(), 8.0, 320, 30.0)
            .unwrap();
        let frame = await_preview(&preview, generation, 8.0).unwrap();
        assert_color(
            frame_mean(&frame),
            3,
            "preview seek past EOF clamps to last picture",
        );
        preview.recycle(frame.rgba);
    }
    preview.stop();
}

#[test]
fn preview_reports_native_errors_and_recovers_on_a_new_request() {
    let fixture = make_color_segments_video();
    let preview = openreel_media::FrameStream::spawn_preview(|| {}).unwrap();
    let bad = fixture.dir.join("not-a-video.mp4");
    std::fs::write(&bad, b"not a media file").unwrap();
    let generation = preview
        .request_preview(bad.to_str().unwrap(), 0.0, 320, 30.0)
        .unwrap();
    let error = await_preview(&preview, generation, 0.0).unwrap_err();
    eprintln!("asynchronous native preview failure: {error}");
    assert!(error.contains("open input"));
    assert!(
        preview.preview_frame(generation, 0.0).is_none(),
        "one failure, not a retry loop"
    );
    let generation = preview
        .request_preview(fixture.path.to_str().unwrap(), 1.0, 320, 30.0)
        .unwrap();
    let frame = await_preview(&preview, generation, 1.0).unwrap();
    assert_color(frame_mean(&frame), 1, "recovery after native open error");
    preview.recycle(frame.rgba);
    preview.cancel_preview();
    assert!(preview.preview_frame(generation, 2.0).is_none());
    assert!(preview
        .request_preview(fixture.path.to_str().unwrap(), f64::NAN, 320, 30.0)
        .is_err());
    preview.stop();
}

#[cfg(unix)]
#[test]
fn preview_coalesces_requests_while_native_open_is_blocked() {
    use std::io::Write;
    use std::sync::mpsc::channel;

    let fixture = make_color_segments_video();
    let slow = fixture.dir.join("blocked.mp4");
    let obsolete = fixture.dir.join("must-not-open.mp4");
    for path in [&slow, &obsolete] {
        assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
    }
    // If a queued obsolete request is opened, its empty FIFO stalls the worker
    // and the final real request times out. Drop the writer before joining so
    // even a failing test releases that native read.
    let obsolete_writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&obsolete)
        .unwrap();
    let (opened_tx, opened_rx) = channel();
    let (release_tx, release_rx) = channel();
    let writer = std::thread::spawn(move || {
        let mut file = std::fs::File::create(slow).unwrap();
        opened_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
        file.write_all(b"invalid input after a blocked open")
            .unwrap();
    });
    let preview = openreel_media::FrameStream::spawn_preview(|| {}).unwrap();
    let first = preview
        .request_preview(
            fixture.dir.join("blocked.mp4").to_str().unwrap(),
            0.0,
            320,
            30.0,
        )
        .unwrap();
    opened_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let replacing = Instant::now();
    preview.cancel_preview();
    for _ in 0..1000 {
        preview
            .request_preview(obsolete.to_str().unwrap(), 0.0, 320, 30.0)
            .unwrap();
    }
    let latest = preview
        .request_preview(fixture.path.to_str().unwrap(), 2.5, 320, 30.0)
        .unwrap();
    let caller_time = replacing.elapsed();
    assert!(preview.preview_frame(first, 0.0).is_none());
    release_tx.send(()).unwrap();
    writer.join().unwrap();
    let result = await_preview(&preview, latest, 2.5);
    drop(obsolete_writer);
    preview.cancel_preview();
    preview.stop();
    eprintln!("cancel + replace 1001 requests while native open is blocked: {caller_time:?}");
    assert!(caller_time < Duration::from_millis(100));
    let frame = result.expect("only the final request may follow the blocked native open");
    assert_color(
        frame_mean(&frame),
        2,
        "latest generation after obsolete requests and error",
    );
}

/// Nearest of COLORS for the inner half of each quadrant of an RGBA picture,
/// in reading order.
fn quadrant_colors(width: u32, height: u32, rgba: &[u8]) -> Vec<&'static str> {
    let (w, h) = (width as usize, height as usize);
    let mut names = Vec::new();
    for (qx, qy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let mut pixels = Vec::new();
        for y in qy * h / 2 + h / 8..qy * h / 2 + 3 * h / 8 {
            for x in qx * w / 2 + w / 8..qx * w / 2 + 3 * w / 8 {
                pixels.extend_from_slice(&rgba[(y * w + x) * 4..(y * w + x) * 4 + 4]);
            }
        }
        let mean = mean_rgb(&pixels);
        let (name, expected) = *COLORS
            .iter()
            .min_by(|a, b| color_distance(mean, a.1).total_cmp(&color_distance(mean, b.1)))
            .unwrap();
        assert!(color_distance(mean, expected) < 48.0, "{mean:?}");
        names.push(name);
    }
    names
}

#[test]
fn native_decoder_and_probe_honor_display_rotation() {
    // Phone footage stores a landscape picture plus a display rotation. The
    // FFmpeg CLI applies it; preview, analysis and model frames must agree.
    let dir = std::env::temp_dir().join(format!(
        "openreel_rotation_{}_{}",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let fixture = Fixture {
        path: dir.join("quadrants.mp4"),
        dir,
    };
    run_ffmpeg(
        Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-filter_complex"])
            .arg(
                "color=red:s=64x32:d=0.5[a];color=lime:s=64x32:d=0.5[b];\
                 color=blue:s=64x32:d=0.5[c];color=white:s=64x32:d=0.5[d];\
                 [a][b][c][d]xstack=inputs=4:layout=0_0|w0_0|0_h0|w0_h0",
            )
            .args(["-c:v", "libx264", "-pix_fmt", "yuv444p", "-qp", "0"])
            .arg(&fixture.path),
    );
    for rotation in ["90", "-90", "180"] {
        let path = fixture.dir.join(format!("rotated{rotation}.mp4"));
        run_ffmpeg(
            Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-display_rotation", rotation, "-i"])
                .arg(&fixture.path)
                .args(["-c", "copy"])
                .arg(&path),
        );
        // Independent oracle: the CLI's autorotated picture.
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .output()
            .unwrap();
        let (width, height) = if rotation == "180" {
            (128, 64)
        } else {
            (64, 128)
        };
        assert_eq!(out.stdout.len(), width * height * 4, "rotation {rotation}");
        let expected = quadrant_colors(width as u32, height as u32, &out.stdout);
        assert_ne!(expected, ["red", "lime", "blue", "white"], "oracle rotated");

        let info = openreel_media::probe(path.to_str().unwrap()).unwrap();
        assert_eq!((info.width, info.height), (width as u32, height as u32));
        let mut decoder = native::VideoDecoder::open(path.to_str().unwrap(), 32, false).unwrap();
        let frame = decoder.frame_at(0.1).unwrap();
        assert_eq!(
            (frame.width, frame.height),
            (32, scaled_height(width as u32, height as u32, 32))
        );
        assert_eq!(
            quadrant_colors(frame.width, frame.height, &frame.rgba),
            expected,
            "rotation {rotation}"
        );
    }
}

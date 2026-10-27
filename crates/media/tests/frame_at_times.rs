//! Pins that `StillDecoder`/`frame_at(t)` returns the frame actually
//! at time `t`, not just a correctly-sized frame. The other media
//! tests assert sizes and counts only. A synthetic source with one
//! solid color per second makes the correct frame unambiguous from
//! its mean color, independent of scaler and colorspace details.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const COLORS: [(&str, (f64, f64, f64)); 4] = [
    ("red", (255.0, 0.0, 0.0)),
    ("lime", (0.0, 255.0, 0.0)),
    ("blue", (0.0, 0.0, 255.0)),
    ("white", (255.0, 255.0, 255.0)),
];

fn make_color_segments_video() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("openreel_frame_at_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("segments.mp4");
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
        "-pix_fmt",
        "yuv420p",
    ])
    .arg(&path);
    let out = cmd.output().expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
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

#[test]
fn still_decoder_returns_frame_at_requested_time() {
    let path = make_color_segments_video();
    let path_str = path.to_str().expect("utf8 path").to_owned();
    let (decoder, rx) = openreel_media::StillDecoder::spawn(|| {});
    for (i, (_, expected)) in COLORS.iter().enumerate() {
        let t = i as f64 + 0.5;
        decoder.request(i as u64 + 1, path_str.clone(), 320, 180, t, 320, false);
        let (id, result) = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("still result");
        assert_eq!(id, i as u64 + 1);
        let frame = result.expect("decode ok");
        assert_eq!((frame.width, frame.height), (320, 180));
        let mean = mean_rgb(&frame.rgba);
        let nearest = COLORS
            .iter()
            .min_by(|a, b| {
                color_distance(mean, a.1)
                    .partial_cmp(&color_distance(mean, b.1))
                    .expect("finite distances")
            })
            .expect("colors nonempty");
        assert!(
            color_distance(mean, nearest.1) == color_distance(mean, *expected),
            "t={t}: expected segment color #{i} near {expected:?}, got mean {mean:?} (nearest {})",
            nearest.0
        );
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("temp dir"));
}

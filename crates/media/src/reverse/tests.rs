use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use super::*;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eddie-reverse-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Two seconds at 30 fps whose frame n has luma 16 + 3n (3 levels apart), over a mono ramp
/// whose sample at time t is t / 4.
fn ramp(dir: &Path) -> String {
    let path = dir.join("ramp.mov");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=64x48:r=30:d=2,format=yuv420p,geq=lum='16+3*N':cb=128:cr=128")
        .args(["-f", "lavfi", "-i", "aevalsrc='t/4':s=48000:d=2"])
        .args(["-c:v", "libx264", "-qp", "0", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    path.to_string_lossy().into_owned()
}

/// Every picture's mean luma, in order.
fn lumas(path: &Path) -> Vec<f64> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .chunks(64 * 48)
        .map(|frame| frame.iter().map(|&v| f64::from(v)).sum::<f64>() / frame.len() as f64)
        .collect()
}

/// The sound as mono floats at 48 kHz.
fn sound(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ac", "1", "-ar", "48000", "-",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect()
}

#[test]
fn a_range_snaps_out_to_whole_frames() {
    assert_eq!(frame_range(30.0, 0.51, 1.49), (0.5, 1.5));
    assert_eq!(frame_range(30.0, 0.5, 1.5), (0.5, 1.5));
    // An empty range still holds one frame.
    assert_eq!(frame_range(25.0, 0.4, 0.4), (0.4, 0.44));
}

#[test]
fn a_reversed_range_plays_its_frames_and_sound_last_first() {
    let dir = folder("range");
    let source = ramp(&dir);
    let output = dir.join("reversed.mov");
    let mut shares = Vec::new();
    // Chunks of a few frames: the join of several segments is under test.
    let cancel = AtomicBool::new(false);
    reverse(
        &source,
        (0.5, 1.5),
        &output,
        &mut |share| shares.push(share),
        &cancel,
        7,
    )
    .unwrap();
    assert_eq!(shares.last(), Some(&1.0));

    let info = crate::probe(&output.to_string_lossy()).unwrap();
    assert_eq!((info.width, info.height), (64, 48));
    assert!((info.fps - 30.0).abs() < 1e-6, "fps {}", info.fps);
    assert!(
        (info.duration - 1.0).abs() < 0.01,
        "duration {}",
        info.duration
    );

    // Frames 15..=44, last first: frame j shows source frame 44 - j.
    let original = lumas(Path::new(&source));
    let reversed = lumas(&output);
    assert_eq!(reversed.len(), 30);
    for (j, luma) in reversed.iter().enumerate() {
        let wanted = original[44 - j];
        // Neighbouring frames are about 3.5 levels apart once decoded.
        assert!(
            (luma - wanted).abs() < 1.5,
            "frame {j}: {luma} against {wanted}"
        );
    }

    // Sound at reversed time r is the source's at 1.5 - r.
    let samples = sound(&output);
    assert!(
        (samples.len() as i64 - 48_000).abs() <= 1,
        "{} samples",
        samples.len()
    );
    for r in [0.05, 0.5, 0.95] {
        let heard = samples[(r * 48_000.0) as usize];
        let wanted = (1.5 - r) / 4.0;
        assert!(
            (f64::from(heard) - wanted).abs() < 0.002,
            "at {r}: {heard} against {wanted}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_cancelled_reversal_leaves_nothing_behind() {
    let dir = folder("cancel");
    let source = ramp(&dir);
    let output = dir.join("reversed.mov");
    let cancel = AtomicBool::new(true);
    let result = reverse_range(&source, 0.0, 2.0, &output, &mut |_| {}, &cancel);
    assert!(result.is_err());
    let left: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["ramp.mov"]);
    let _ = fs::remove_dir_all(&dir);
}

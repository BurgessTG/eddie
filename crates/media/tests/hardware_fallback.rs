//! A hardware decoder that fails after attachment falls back to software
//! for the session. Its own test binary: the fault hook is process-wide.

use std::process::Command;
use std::time::Duration;

use openreel_media::{FrameStream, RawFrame};

fn mean_red_blue(frame: &RawFrame) -> (f64, f64) {
    let pixels = frame.rgba.len() as f64 / 4.0;
    let sum = |channel: usize| -> f64 {
        frame
            .rgba
            .chunks_exact(4)
            .map(|pixel| f64::from(pixel[channel]))
            .sum::<f64>()
            / pixels
    };
    (sum(0), sum(2))
}

#[test]
fn a_failing_hardware_decoder_continues_playback_in_software() {
    let dir = std::env::temp_dir().join(format!("openreel_hw_fallback_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("red-blue.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args(["-f", "lavfi", "-i", "color=c=red:s=320x180:r=10:d=1"])
        .args(["-f", "lavfi", "-i", "color=c=blue:s=320x180:r=10:d=1"])
        .args(["-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0[v]"])
        .args(["-map", "[v]", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let path = path.to_str().unwrap();

    openreel_media::inject_hardware_fault(true);
    let preview = FrameStream::spawn_preview(|| {}).unwrap();
    let generation = preview.request_preview(path, 0.0, 320, 10.0).unwrap();
    // The "hardware" decoder fails on its sixth picture, inside the red
    // second; playback carries on from the clock in software. The clock
    // moves like a UI's, whether or not a new picture arrived.
    let mut shown = Vec::new();
    let mut time: f64 = 0.0;
    while time < 1.9 {
        if let Some(result) = preview.preview_frame(generation, time) {
            let frame = result.unwrap_or_else(|error| panic!("no fallback at {time}: {error}"));
            let (red, blue) = mean_red_blue(&frame);
            shown.push((time, red > blue + 100.0, blue > red + 100.0));
            preview.recycle(frame.rgba);
        }
        std::thread::sleep(Duration::from_millis(5));
        time += 0.01;
    }
    // A picture may arrive a little after its time, never early.
    assert!(
        shown.iter().all(|&(at, red, blue)| if at < 0.95 {
            red
        } else if at > 1.15 {
            blue
        } else {
            red || blue
        }),
        "{shown:?}"
    );
    assert!(
        shown.iter().filter(|&&(at, ..)| at > 1.2).count() >= 4,
        "{shown:?}"
    );
    let failure = openreel_media::take_hardware_failure();
    assert_eq!(failure.as_deref(), Some("injected hardware decode failure"));
    assert_eq!(
        openreel_media::take_hardware_failure(),
        None,
        "reported once"
    );
    openreel_media::inject_hardware_fault(false);
    preview.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

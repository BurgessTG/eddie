//! One-off pictures: a single still and an import's filmstrip.

use crate::{decode, run, scaled_height, MediaResult, RawFrame};

/// Decode one frame at `time` (seconds), scaled to `target_w`.
/// `fast` trades scaler quality for speed (used while scrubbing).
/// Uses the in-process decoder first, falling back to the FFmpeg CLI for
/// anything libavcodec rejects in this build.
pub fn frame_at(
    path: &str,
    src_w: u32,
    src_h: u32,
    time: f64,
    target_w: u32,
    fast: bool,
) -> MediaResult<RawFrame> {
    match decode::frame_at(path, time, target_w, fast) {
        Ok(frame) if frame.height == scaled_height(src_w, src_h, target_w) => Ok(frame),
        _ => frame_at_cli(path, src_w, src_h, time, target_w, fast),
    }
}

fn frame_at_cli(
    path: &str,
    src_w: u32,
    src_h: u32,
    time: f64,
    target_w: u32,
    fast: bool,
) -> MediaResult<RawFrame> {
    let target_h = scaled_height(src_w, src_h, target_w);
    let flags = if fast { "fast_bilinear" } else { "bicubic" };
    let output = run(crate::cli::ffmpeg().args([
        "-v",
        "error",
        "-ss",
        &format!("{time:.3}"),
        "-i",
        path,
        "-frames:v",
        "1",
        "-vf",
        &format!("scale={target_w}:{target_h}:flags={flags}"),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-",
    ]))?;
    let expected = (target_w * target_h * 4) as usize;
    if output.stdout.len() < expected {
        return Err(format!(
            "short frame from ffmpeg: {} of {expected} bytes",
            output.stdout.len()
        ));
    }
    Ok(RawFrame {
        width: target_w,
        height: target_h,
        rgba: output.stdout[..expected].to_vec(),
    })
}

/// Filmstrip thumbnails: one frame every `interval` seconds (widened so the
/// count stays bounded on long files). Returns `(source_time, frame)`.
pub fn thumbnails(
    path: &str,
    src_w: u32,
    src_h: u32,
    duration: f64,
) -> MediaResult<Vec<(f64, RawFrame)>> {
    if scaled_height(src_w, src_h, 144) == 0 {
        return Ok(Vec::new());
    }
    let interval = (duration / 90.0).max(2.0);
    let count = (duration / interval).ceil().max(1.0) as usize;
    let times: Vec<f64> = (0..count).map(|i| i as f64 * interval).collect();
    let frames = decode::filmstrip_stills(path, &times, 144, interval)?;
    if frames.is_empty() {
        return Err("thumbnail decode failed".to_owned());
    }
    Ok(frames)
}

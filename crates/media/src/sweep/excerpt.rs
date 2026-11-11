//! Sound excerpts for speech models.

use std::path::Path;
use std::process::Command;

use crate::{run, MediaResult};

/// Keeps an excerpt's first sample at the time asked for. Without it FFmpeg
/// starts the output at the first sound it finds, so sound that begins after
/// the picture (or after a gap) came out early, and a caller advancing by
/// what it received read the same sound twice. Silence fills the lead and
/// any gap of 0.1 s or more.
const EXCERPT_ON_ITS_CLOCK: &str = "aresample=async=1:first_pts=0";

/// Extract a mono 24 kHz PCM WAV chunk in the shape Meta's batch ASR accepts.
/// Metadata is stripped so caller context does not ride along in the upload.
pub fn write_wav_24k(path: &str, start: f64, duration: f64, output: &Path) -> MediaResult<()> {
    run(Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-ss",
            &format!("{:.3}", start.max(0.0)),
            "-t",
            &format!("{:.3}", duration.max(0.0)),
            "-i",
            path,
            "-vn",
            "-af",
            EXCERPT_ON_ITS_CLOCK,
            "-ac",
            "1",
            "-ar",
            "24000",
            "-c:a",
            "pcm_s16le",
            "-map_metadata",
            "-1",
            "-f",
            "wav",
        ])
        .arg(output))?;
    Ok(())
}

/// Decode a mono 16 kHz float PCM excerpt, the input local speech models
/// expect. Bounded by the caller's duration; never a whole-file decode.
pub fn pcm_mono_16k(path: &str, start: f64, duration: f64) -> MediaResult<Vec<f32>> {
    let output = run(Command::new("ffmpeg").args([
        "-v",
        "error",
        "-ss",
        &format!("{:.3}", start.max(0.0)),
        "-t",
        &format!("{:.3}", duration.max(0.0)),
        "-i",
        path,
        "-vn",
        "-af",
        EXCERPT_ON_ITS_CLOCK,
        "-ac",
        "1",
        "-ar",
        "16000",
        "-f",
        "f32le",
        "-",
    ]))?;
    Ok(output
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect())
}

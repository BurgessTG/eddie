//! Generates real sample media with FFmpeg's lavfi sources so a fresh
//! install has something to chop up immediately.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{run, MediaResult};

/// Create `sample_video.mp4` (720p test pattern + tone, 20s) and
/// `sample_music.mp3` (a sine melody, ~14s) in `dir` if missing.
/// Returns the paths, video first.
pub fn generate_samples(dir: &Path) -> MediaResult<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create samples dir: {e}"))?;
    let video = dir.join("sample_video.mp4");
    let music = dir.join("sample_music.mp3");

    if !is_nonempty(&video) {
        run(Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=duration=20:size=1280x720:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:duration=20",
                "-af",
                // lavfi sine peaks at 0.125, so this lands near -8 dB.
                "volume=3.0",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "128k",
                "-shortest",
            ])
            .arg(&video))?;
    }

    if !is_nonempty(&music) {
        // Two passes over a C-major-ish melody, 0.9s per note.
        let notes = [
            261.63, 329.63, 392.00, 523.25, 392.00, 329.63, 293.66, 392.00,
        ];
        let sequence: Vec<f64> = notes.iter().chain(notes.iter()).copied().collect();
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-y", "-v", "error"]);
        for freq in &sequence {
            cmd.args([
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency={freq:.2}:duration=0.9"),
            ]);
        }
        let concat_inputs: String = (0..sequence.len()).map(|i| format!("[{i}:a]")).collect();
        cmd.args([
            "-filter_complex",
            &format!(
                "{concat_inputs}concat=n={}:v=0:a=1,volume=6.0[a]",
                sequence.len()
            ),
            "-map",
            "[a]",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "160k",
        ])
        .arg(&music);
        run(&mut cmd)?;
    }

    Ok(vec![video, music])
}

fn is_nonempty(path: &Path) -> bool {
    path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

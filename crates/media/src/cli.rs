//! The `ffmpeg` command-line tool, for the work not yet done in process.

use std::process::{Command, Output};

use crate::MediaResult;

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

pub(crate) fn run(command: &mut Command) -> MediaResult<Output> {
    let output = command
        .output()
        .map_err(|e| format!("failed to launch tool: {e}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(4).collect();
        Err(format!(
            "tool exited with {}: {}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" ")
        ))
    }
}

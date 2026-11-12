//! The `ffmpeg` command-line tool, for the work not yet done in process.

use std::ffi::OsString;
use std::process::{Command, Output};
use std::sync::OnceLock;

use crate::MediaResult;

/// A command for the `ffmpeg` tool: the one `EDDIE_FFMPEG` names, else the
/// one found for this system.
pub fn ffmpeg() -> Command {
    static PROGRAM: OnceLock<OsString> = OnceLock::new();
    Command::new(PROGRAM.get_or_init(|| {
        std::env::var_os("EDDIE_FFMPEG")
            .filter(|program| !program.is_empty())
            .unwrap_or_else(find)
    }))
}

/// Elsewhere than macOS the tool is whatever `ffmpeg` PATH finds.
#[cfg(not(target_os = "macos"))]
fn find() -> OsString {
    "ffmpeg".into()
}

/// On macOS PATH is not enough. An app started from the Finder gets a PATH
/// without Homebrew, and Homebrew's `ffmpeg` has no `drawtext` filter, which
/// titles and contact sheets need; its `ffmpeg-full` has one and is not on
/// PATH. So the first of these that draws text is used, else the first that
/// runs at all.
#[cfg(target_os = "macos")]
fn find() -> OsString {
    const PLACES: [&str; 5] = [
        "ffmpeg",
        "/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg",
        "/usr/local/opt/ffmpeg-full/bin/ffmpeg",
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
    ];
    let mut runs = None;
    for place in PLACES {
        let filters = Command::new(place)
            .args(["-v", "quiet", "-filters"])
            .output();
        let Some(filters) = filters.ok().filter(|out| out.status.success()) else {
            continue;
        };
        if String::from_utf8_lossy(&filters.stdout).contains(" drawtext ") {
            return place.into();
        }
        runs.get_or_insert(place);
    }
    runs.unwrap_or(PLACES[0]).into()
}

/// What a failed run means when the tool itself is the problem: an FFmpeg
/// built without FreeType has no `drawtext` filter, which draws titles and
/// the labels on sheets and strips. `None` for every other failure.
pub(crate) fn cannot_draw_text(stderr: &str) -> Option<String> {
    stderr.contains("No such filter: 'drawtext'").then(|| {
        "This FFmpeg cannot draw text, so titles and sheet labels cannot be made. \
         Install one with the drawtext filter (Homebrew: ffmpeg-full) or name one in EDDIE_FFMPEG."
            .to_owned()
    })
}

pub fn ffmpeg_available() -> bool {
    ffmpeg().arg("-version").output().is_ok()
}

pub(crate) fn run(command: &mut Command) -> MediaResult<Output> {
    let output = command
        .output()
        .map_err(|e| format!("failed to launch tool: {e}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(reason) = cannot_draw_text(&stderr) {
            return Err(reason);
        }
        let tail: Vec<&str> = stderr.lines().rev().take(4).collect();
        Err(format!(
            "tool exited with {}: {}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" ")
        ))
    }
}

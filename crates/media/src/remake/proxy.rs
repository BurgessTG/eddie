//! Proxies, as CapCut's Proxy setting makes them: a light copy of a video
//! for the preview to play and scrub instead of the file. Its picture is
//! the preview's width, upright, 8-bit 4:2:0 H.264 with a keyframe every
//! half second and no B-frames, and every picture keeps the original's
//! time, so a decoder of the copy shows what one of the original shows at
//! the same source time. Export and the picture commands keep reading the
//! original.

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;

use super::{collect_stderr, finish, threads, watch, with_stderr};
use crate::export::ScratchDir;
use crate::{scaled_height, MediaResult};

/// x264's quality: export's Recommended, at the preview's size.
const CRF: &str = "20";

/// Write a proxy of `source`'s picture, `width` pixels wide as displayed
/// (never wider than it is), to `output`. The file appears whole or not at
/// all. `progress` hears the share done; setting `cancel` stops the work
/// with an error within a tenth of a second. FFmpeg runs at a lower
/// priority, so playback comes first, and on Linux it dies with this
/// process.
pub fn make_proxy(
    source: &str,
    width: u32,
    output: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> MediaResult<()> {
    let info = crate::probe(source)?;
    if !info.has_video || info.photo {
        return Err(format!("{source} has no footage to make a proxy of"));
    }
    let width = width.min(info.width) & !1;
    let height = scaled_height(info.width, info.height, width);
    let scratch = ScratchDir::beside(output)?;
    let partial = scratch.0.join("proxy.mov");
    let frames = (info.duration * info.fps).round().max(1.0) as usize;
    let keyframes = (info.fps / 2.0).round().max(1.0).to_string();
    let mut command = crate::cli::ffmpeg();
    command
        .args(["-v", "error", "-nostdin", "-nostats", "-y"])
        .args(["-threads", &threads(), "-i"])
        .arg(source)
        // A moment of silence from 0 keeps the copy's start where the
        // original's is: without it, a file whose sound starts before its
        // picture had its first picture moved to 0.
        .args(["-f", "lavfi", "-t", "0.05", "-i", "anullsrc=r=8000:cl=mono"])
        .args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "pcm_s16le"])
        .arg("-vf")
        .arg(format!(
            "scale={width}:{height}:flags=bicubic,format=yuv420p"
        ))
        // Each picture at its own time, in the original's time base.
        .args(["-fps_mode", "passthrough", "-enc_time_base", "demux"])
        .args(["-c:v", "libx264", "-threads:v", &threads()])
        // veryfast: on 11.5 s of Sony 4K, files a third of ultrafast's
        // (3.1 MB against 9.1 MB), made as fast (7.8 s against 7.5 s,
        // decoding the original is the work) and decoded in 1.0 ms a
        // picture against 0.7 ms.
        .args(["-preset", "veryfast", "-crf", CRF, "-bf", "0"])
        .args(["-g", &keyframes, "-f", "mov"])
        .args(["-progress", "pipe:1", "-stats_period", "0.1"])
        .arg(&partial)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    background(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let errors = collect_stderr(child.stderr.take());
    if let Err(error) = watch(&mut child, frames, progress, cancel) {
        let _ = child.kill();
        let _ = child.wait();
        let stderr = errors.join().unwrap_or_default();
        return Err(with_stderr(error, &stderr));
    }
    finish(&mut child, errors, "making the proxy")?;
    fs::rename(&partial, output).map_err(|e| format!("save {}: {e}", output.display()))?;
    progress(1.0);
    Ok(())
}

/// Start `command` at nice 10, so it gets about a tenth of a busy
/// processor next to playback and all of an idle one. On Linux it is
/// also killed when the thread that started it ends, which, as that
/// thread waits for it, happens only when this process ends: a proxy is
/// one long FFmpeg run that would otherwise go on alone.
#[cfg(unix)]
fn background(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn setpriority(which: i32, who: u32, priority: i32) -> i32;
        #[cfg(target_os = "linux")]
        fn prctl(option: i32, arg: u64, ...) -> i32;
    }
    // SAFETY: setpriority and prctl are system calls that read no memory,
    // safe between fork and exec. PRIO_PROCESS (0) with 0 is the new
    // process; PR_SET_PDEATHSIG is 1 and SIGKILL 9.
    unsafe {
        command.pre_exec(|| {
            setpriority(0, 0, 10);
            #[cfg(target_os = "linux")]
            prctl(1, 9);
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn background(_command: &mut std::process::Command) {}

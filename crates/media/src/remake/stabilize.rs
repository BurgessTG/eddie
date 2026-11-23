//! A range steadied with vid.stab, as Shotcut and Kdenlive do it, in two
//! passes over the same frames. The first measures how each frame moved
//! from the one before (`vidstabdetect`) on a copy at most
//! [`DETECT_SIDE`] wide or tall, and the measured motions are scaled up to
//! the full picture: on 4K footage that took 7.2 s instead of 25 s and
//! steadied it exactly as well. The second smooths the camera's path,
//! moves each frame onto it and enlarges the picture to keep its edges
//! covered, as `vidstabtransform` does but on every core (`warp`), into
//! the encoder.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, sync_channel};
use std::thread;

use openreel_engine::Stabilize;

use super::{
    cancelled, collect_stderr, encoder, end_encoder, finish, run, threads, warp, watch,
    with_stderr, Picture, Source,
};
use crate::export::{filter_value, standard_picture};
use crate::MediaResult;

/// The longest side motion is measured at.
pub(super) const DETECT_SIDE: u32 = 1920;
/// The share of the work the first pass takes.
const DETECTING: f64 = 0.3;

/// Write `[start, end)` of `source`, steadied as `level` says, to
/// `partial`, measuring its motion at most `detect_side` pixels wide or
/// tall.
#[allow(clippy::too_many_arguments)]
pub(super) fn stabilize(
    source: &Source,
    (start, end): (f64, f64),
    level: Stabilize,
    scratch: &Path,
    partial: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
    detect_side: u32,
) -> MediaResult<()> {
    let picture = source
        .picture
        .ok_or_else(|| format!("{} has no picture to stabilize", source.path))?;
    let frames = picture.frames(start, end);
    let sound = source.sound((start, end), false, scratch)?;
    progress(0.02);

    let motions = scratch.join("motions.trf");
    let (scale_x, scale_y) = detect(
        source,
        picture,
        start,
        frames,
        detect_side,
        &motions,
        &mut |share| progress(0.02 + DETECTING * share),
        cancel,
    )?;
    if (scale_x, scale_y) != (1.0, 1.0) {
        let measured = std::fs::read_to_string(&motions)
            .map_err(|e| format!("reading the measured motion: {e}"))?;
        std::fs::write(&motions, scale_motions(&measured, scale_x, scale_y)?)
            .map_err(|e| format!("writing the measured motion: {e}"))?;
    }

    let (encoder, input, errors) = encoder(picture, sound.as_deref(), partial)?;
    let fed = steady(
        source,
        picture,
        start,
        frames,
        level,
        &motions,
        input,
        &mut |share| progress(0.02 + DETECTING + (0.97 - DETECTING) * share),
        cancel,
    );
    end_encoder(encoder, errors, fed, "encoding the stabilized picture")
}

/// The filters both passes start with, so they see the same frames: the
/// picture cropped to even sides and held at its end if the file runs out
/// first.
fn frames_filter(picture: Picture) -> String {
    format!(
        "crop={}:{}:0:0,tpad=stop_mode=clone:stop=-1",
        picture.width, picture.height
    )
}

/// The first pass: measure each frame's motion into `motions`, from a copy
/// at most `detect_side` on its longer side. Returns how much larger the
/// picture is than that copy, across and down.
#[allow(clippy::too_many_arguments)]
fn detect(
    source: &Source,
    picture: Picture,
    start: f64,
    frames: usize,
    detect_side: u32,
    motions: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> MediaResult<(f64, f64)> {
    let (width, height) = (picture.width, picture.height);
    let shrink = (f64::from(width.max(height)) / f64::from(detect_side)).max(1.0);
    let even = |side: u32| ((f64::from(side) / shrink / 2.0).round() as u32 * 2).max(2);
    let (small_w, small_h) = if shrink > 1.0 {
        (even(width), even(height))
    } else {
        (width, height)
    };
    let mut filters = frames_filter(picture);
    if (small_w, small_h) != (width, height) {
        write!(filters, ",scale={small_w}:{small_h}").unwrap();
    }
    write!(
        filters,
        ",vidstabdetect=shakiness=6:fileformat=ascii:result={}",
        filter_value(&motions.to_string_lossy())
    )
    .unwrap();
    let mut command = crate::cli::ffmpeg();
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-nostats",
            "-threads",
            &threads(),
        ])
        .args(["-ss", &source.seek(start), "-i"])
        .arg(&source.path)
        .args(["-map", "0:v:0", "-an", "-sn", "-vf", &filters])
        .args(["-frames:v", &frames.to_string()])
        .args(["-progress", "pipe:1", "-f", "null", "-"]);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let errors = collect_stderr(child.stderr.take());
    let watched = watch(&mut child, frames, progress, cancel);
    if let Err(error) = watched {
        let _ = child.kill();
        let _ = child.wait();
        let stderr = errors.join().unwrap_or_default();
        return Err(with_stderr(error, &stderr));
    }
    finish(&mut child, errors, "measuring the camera's motion").map_err(explain)?;
    Ok((
        f64::from(width) / f64::from(small_w),
        f64::from(height) / f64::from(small_h),
    ))
}

/// The second pass: the camera path vid.stab smooths from `motions`,
/// then the frames decoded again, each moved onto that path here
/// (`warp`) and written to the encoder's `input`.
#[allow(clippy::too_many_arguments)]
fn steady(
    source: &Source,
    picture: Picture,
    start: f64,
    frames: usize,
    level: Stabilize,
    motions: &Path,
    input: ChildStdin,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> MediaResult<()> {
    // vid.stab's smoothing is half the frames its window spans.
    let smoothing = (level.smoothing() * picture.fps() / 2.0)
        .round()
        .clamp(1.0, 1000.0);
    let (shift, angle) = level.limits();
    let shift = (shift * f64::from(picture.width.min(picture.height))).round();
    let options = format!("smoothing={smoothing}:optzoom=1:maxshift={shift}:maxangle={angle}");
    let path = warp::camera_path(
        &frame_motions(picture, motions, &options)?,
        &warp::PathOptions {
            smoothing: smoothing as usize,
            max_shift: shift as i32,
            max_angle: angle,
            width: picture.width,
            height: picture.height,
        },
    );
    if path.is_empty() {
        return Err("no camera motion was measured".to_owned());
    }
    let filters = format!(
        "{},{}",
        frames_filter(picture),
        standard_picture(&source.path)
    );
    let mut decoder = crate::cli::ffmpeg()
        .args(["-v", "error", "-nostdin", "-threads", &threads()])
        .args(["-ss", &source.seek(start), "-i"])
        .arg(&source.path)
        .args(["-map", "0:v:0", "-an", "-sn", "-vf", &filters])
        .args(["-frames:v", &frames.to_string()])
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let errors = collect_stderr(decoder.stderr.take());
    let decoded = decoder.stdout.take().expect("piped decoder output");
    let moved = move_frames(decoded, input, picture, frames, &path, progress, cancel);
    if let Err(error) = moved {
        let _ = decoder.kill();
        let _ = decoder.wait();
        let stderr = errors.join().unwrap_or_default();
        return Err(explain(with_stderr(error, &stderr)));
    }
    finish(&mut decoder, errors, "decoding the picture").map_err(explain)
}

/// How each frame moved from the one before, as vid.stab's transform
/// filter reads `motions`: it writes them to `global_motions.trf` in its
/// working folder when debugging, run on one blank frame of the picture's
/// size (0.13 s).
pub(super) fn frame_motions(
    picture: Picture,
    motions: &Path,
    options: &str,
) -> MediaResult<Vec<warp::Motion>> {
    let folder = motions.parent().unwrap_or(Path::new("."));
    let written = folder.join("global_motions.trf");
    let (rate_num, rate_den) = picture.rate;
    let mut command = crate::cli::ffmpeg();
    command
        .current_dir(folder)
        .args(["-v", "error", "-nostdin", "-f", "lavfi", "-i"])
        .arg(format!(
            "color=c=black:s={}x{}:r={rate_num}/{rate_den}",
            picture.width, picture.height
        ))
        .args(["-frames:v", "1", "-vf"])
        .arg(format!(
            "format=yuv420p,vidstabtransform=input={}:{options}:debug=1",
            filter_value(&motions.to_string_lossy())
        ))
        .args(["-f", "null", "-"]);
    run(command, "reading the camera's motion").map_err(explain)?;
    let text = std::fs::read_to_string(&written)
        .map_err(|e| format!("reading the camera's motion: {e}"))?;
    warp::read_motions(&text)
}

/// Read each of `frames` decoded pictures, move it onto `path` and write
/// it to the encoder's `input`. Reading and writing run on threads of
/// their own, a few frames ahead and behind, while a frame is moved on
/// every core.
fn move_frames(
    decoded: ChildStdout,
    input: ChildStdin,
    picture: Picture,
    frames: usize,
    path: &[warp::Motion],
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> MediaResult<()> {
    let size = (picture.width as usize, picture.height as usize);
    let bytes = picture.frame_bytes();
    let cores = thread::available_parallelism().map_or(4, |n| n.get());
    let (read_tx, read_rx) = sync_channel::<Vec<u8>>(2);
    let (write_tx, write_rx) = sync_channel::<Vec<u8>>(2);
    let (spare_tx, spare_rx) = channel::<Vec<u8>>();
    thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let mut decoded = decoded;
            for _ in 0..frames {
                let mut frame = spare_rx.try_recv().unwrap_or_else(|_| vec![0; bytes]);
                decoded
                    .read_exact(&mut frame)
                    .map_err(|e| format!("decoding the picture: {e}"))?;
                if read_tx.send(frame).is_err() {
                    break;
                }
            }
            Ok(())
        });
        let writer = scope.spawn(move || {
            let mut input = input;
            for frame in write_rx {
                input
                    .write_all(&frame)
                    .map_err(|e| format!("encoding the stabilized picture: {e}"))?;
                let _ = spare_tx.send(frame);
            }
            Ok(())
        });
        let mut moved = Ok(());
        // What the frame before showed, where a moved frame leaves a
        // border: the first frame as it is, as vid.stab starts.
        let mut kept: Option<Vec<u8>> = None;
        for (index, mut frame) in read_rx.iter().enumerate() {
            if let Err(error) = cancelled(cancel) {
                moved = Err(error);
                break;
            }
            let kept = kept.get_or_insert_with(|| frame.clone());
            // Past the measured frames vid.stab keeps the last motion.
            warp::warp(&frame, kept, size, path[index.min(path.len() - 1)], cores);
            frame.copy_from_slice(kept);
            if write_tx.send(frame).is_err() {
                break;
            }
            progress((index + 1) as f64 / frames as f64);
        }
        drop(read_rx);
        drop(write_tx);
        let joined = |handle: thread::ScopedJoinHandle<'_, MediaResult<()>>| {
            handle
                .join()
                .unwrap_or_else(|_| Err("a stabilizing thread failed".to_owned()))
        };
        let (read, written) = (joined(reader), joined(writer));
        moved.and(written).and(read)
    })
}

/// An FFmpeg without vid.stab says so in words a user can act on.
fn explain(error: String) -> String {
    if error.contains("No such filter: 'vidstab") {
        "This FFmpeg has no vid.stab (libvidstab), which Stabilize needs. \
         Install one built with it or name one in EDDIE_FFMPEG."
            .to_owned()
    } else {
        error
    }
}

/// vid.stab's ASCII motions measured on a copy of the picture, scaled up
/// to a picture `scale_x` times as wide and `scale_y` times as tall. Each
/// `(LM vx vy fx fy size contrast match)` is one field's motion, its
/// centre and its size in pixels, then two measures that keep their
/// values.
pub(super) fn scale_motions(text: &str, scale_x: f64, scale_y: f64) -> MediaResult<String> {
    let broken = || "the measured motion could not be read".to_owned();
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut rest = text;
    while let Some(at) = rest.find("(LM ") {
        out.push_str(&rest[..at]);
        let body = &rest[at + 4..];
        let end = body.find(')').ok_or_else(broken)?;
        let fields: Vec<&str> = body[..end].split_whitespace().collect();
        let [vx, vy, fx, fy, size, contrast, matched] = fields[..] else {
            return Err(broken());
        };
        let scaled = |value: &str, scale: f64| -> MediaResult<i64> {
            value
                .parse::<f64>()
                .map(|v| (v * scale).round() as i64)
                .map_err(|_| broken())
        };
        write!(
            out,
            "(LM {} {} {} {} {} {contrast} {matched})",
            scaled(vx, scale_x)?,
            scaled(vy, scale_y)?,
            scaled(fx, scale_x)?,
            scaled(fy, scale_y)?,
            scaled(size, scale_x.min(scale_y))?,
        )
        .unwrap();
        rest = &body[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

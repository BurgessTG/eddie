//! A GIF from the rendered picture segments. One pass finds the 256 colours
//! the whole picture uses most; then every segment is mapped onto them at
//! once, each in its own FFmpeg, and the parts are joined into one GIF. One
//! palette for the whole GIF keeps a colour from flickering between frames,
//! and nothing holds more than one segment's frames.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

use super::export_error;
use crate::MediaResult;

/// The rendered picture as RGB. Export writes BT.709 limited range, read
/// with the same accuracy export converts with.
const TO_RGB: &str =
    "scale=in_color_matrix=bt709:in_range=limited:flags=accurate_rnd+full_chroma_int,format=bgra";

/// [`TO_RGB`] at half the size, each pixel the average of four: what the
/// palette is chosen from.
const HALF_RGB: &str = "scale=iw/2:ih/2:in_color_matrix=bt709:in_range=limited:\
flags=area+accurate_rnd+full_chroma_int,format=bgra";

/// At most this many frames choose the palette. Its pass is single-threaded
/// and slows as colours pile up: on 185 s of 4K footage at 480p, every
/// frame took 51.7 s, every eighth frame at half size 4.5 s, and the GIF
/// was as close to the picture (PSNR 21.62 dB against 21.61).
const PALETTE_FRAMES: i64 = 360;

/// At most this many segments are mapped at once, each FFmpeg holding
/// about 90 MB at 480p. On 185 s at 480p with 16 threads, eight took 63.8 s
/// at 983 MB peak; sixteen 61.8 s at 1.63 GB.
const MAPPERS: usize = 8;

/// Write `output` from the `segments` H.264 files in `scratch` (`00000.mp4`
/// on, listed in `list`), `frames` frames at `rate` in all. Progress runs
/// from `from` to 0.99. The GIF loops forever, as GIFs are expected to.
pub(super) fn write(
    scratch: &Path,
    list: &Path,
    segments: usize,
    (frames, rate): (i64, ffmpeg_next::Rational),
    (progress, from): (&Sender<f64>, f64),
    output: &Path,
) -> MediaResult<()> {
    let palette = scratch.join("palette.png");
    let step = (frames / PALETTE_FRAMES).max(1);
    run(crate::cli::ffmpeg()
        .args(["-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(list)
        // Every entry is a colour: nothing in an export is transparent.
        .arg("-vf")
        .arg(format!(
            "select='not(mod(n\\,{step}))',{HALF_RGB},palettegen=reserve_transparent=0"
        ))
        .args(["-fps_mode", "vfr"])
        .arg(&palette))?;
    let mapping = from + (0.99 - from) * 0.1;
    let _ = progress.send(mapping);

    let part = |index: usize| scratch.join(format!("{index:05}.gif"));
    let next = AtomicUsize::new(0);
    let mapped = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let workers = thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(MAPPERS)
        .min(segments);
    thread::scope(|scope| {
        let workers: Vec<_> = (0..workers)
            .map(|_| {
                let progress = progress.clone();
                scope.spawn(|| -> MediaResult<()> {
                    let progress = progress;
                    while !failed.load(Ordering::Relaxed) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= segments {
                            break;
                        }
                        // Only the part of a frame that changed is mapped
                        // again, so a still background keeps its dither.
                        let result = run(crate::cli::ffmpeg()
                            .args(["-y", "-v", "error", "-threads", "1", "-i"])
                            .arg(scratch.join(format!("{index:05}.mp4")))
                            .arg("-i")
                            .arg(&palette)
                            .args(["-filter_threads", "1", "-lavfi"])
                            .arg(format!(
                                "[0:v]{TO_RGB}[rgb];[rgb][1:v]paletteuse=dither=sierra2_4a:diff_mode=rectangle"
                            ))
                            .args(["-f", "gif"])
                            .arg(part(index)));
                        if result.is_err() {
                            failed.store(true, Ordering::Relaxed);
                            return result;
                        }
                        let done = mapped.fetch_add(1, Ordering::Relaxed) + 1;
                        let share = done as f64 / segments as f64;
                        let _ = progress.send(mapping + (0.99 - mapping) * share);
                    }
                    Ok(())
                })
            })
            .collect();
        workers
            .into_iter()
            .try_for_each(|worker| worker.join().expect("GIF worker panicked"))
    })?;
    join((0..segments).map(part), rate, output)
}

/// Join GIFs that share one screen and palette into `output`: the first
/// one's header and palette, one block that loops forever, every part's
/// frames, then one trailer. Each frame's delay is set again from its place
/// in the whole GIF, so the rounding to hundredths of a second never adds
/// up across parts. Each part is removed once it is in.
fn join(
    parts: impl Iterator<Item = std::path::PathBuf>,
    rate: ffmpeg_next::Rational,
    output: &Path,
) -> MediaResult<()> {
    let write_error = |e: std::io::Error| format!("write the GIF: {e}");
    let mut out = BufWriter::new(File::create(output).map_err(write_error)?);
    let mut screen: Option<Vec<u8>> = None;
    let mut frame = 0_i64;
    // Hundredths of a second from the start to frame `n`.
    let at = |n: i64| {
        (100 * n * i64::from(rate.denominator()) * 2 + i64::from(rate.numerator()))
            / (2 * i64::from(rate.numerator()))
    };
    for path in parts {
        let bytes = fs::read(&path).map_err(|e| format!("read a GIF part: {e}"))?;
        let bad = || "FFmpeg wrote a GIF part Eddie cannot read".to_owned();
        let header = 13
            + bytes
                .get(10)
                .filter(|flags| *flags & 0x80 != 0)
                .map_or(0, |flags| 3 << ((flags & 7) + 1));
        let head = bytes.get(..header).ok_or_else(bad)?;
        match &screen {
            None => {
                out.write_all(head).map_err(write_error)?;
                out.write_all(b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00")
                    .map_err(write_error)?;
                screen = Some(head[6..].to_vec());
            }
            Some(first) if *first != head[6..] => {
                return Err("the GIF's parts have different palettes".to_owned());
            }
            Some(_) => {}
        }
        let mut at_byte = header;
        loop {
            match *bytes.get(at_byte).ok_or_else(bad)? {
                0x3B => break,
                0x21 => {
                    let label = *bytes.get(at_byte + 1).ok_or_else(bad)?;
                    let end = sub_blocks(&bytes, at_byte + 2).ok_or_else(bad)?;
                    let mut block = bytes[at_byte..end].to_vec();
                    if label == 0xF9 && block.len() >= 6 {
                        let delay = (at(frame + 1) - at(frame)).clamp(0, 0xFFFF) as u16;
                        block[4..6].copy_from_slice(&delay.to_le_bytes());
                    }
                    // Each part says to loop; the joined GIF says it once.
                    if !(label == 0xFF && block.get(3..14) == Some(b"NETSCAPE2.0")) {
                        out.write_all(&block).map_err(write_error)?;
                    }
                    at_byte = end;
                }
                0x2C => {
                    let flags = *bytes.get(at_byte + 9).ok_or_else(bad)?;
                    let table = if flags & 0x80 != 0 {
                        3 << ((flags & 7) + 1)
                    } else {
                        0
                    };
                    // The LZW code size, then the picture's sub-blocks.
                    let end = sub_blocks(&bytes, at_byte + 10 + table + 1).ok_or_else(bad)?;
                    out.write_all(&bytes[at_byte..end]).map_err(write_error)?;
                    frame += 1;
                    at_byte = end;
                }
                _ => return Err(bad()),
            }
        }
        let _ = fs::remove_file(&path);
    }
    out.write_all(&[0x3B]).map_err(write_error)?;
    out.flush().map_err(write_error)
}

/// The end of the sub-blocks that start at `at`: each a length byte and
/// that many bytes, closed by a zero length.
fn sub_blocks(bytes: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let length = usize::from(*bytes.get(at)?);
        at += 1 + length;
        if length == 0 {
            return (at <= bytes.len()).then_some(at);
        }
    }
}

fn run(command: &mut std::process::Command) -> MediaResult<()> {
    let out = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(export_error(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

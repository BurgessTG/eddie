//! Clips played backwards or stabilized, as CapCut makes them: a range of
//! a file written into a file of its own, which preview, export and
//! pictures then read like any other ([`openreel_engine::Remade`]).
//!
//! Both kinds end in one x264 encode fed raw pictures by this process, and
//! sound decoded to 16-bit PCM at 48 kHz. [`reverse`] decodes the picture
//! in chunks from the last to the first; [`stabilize`] measures the
//! camera's motion with vid.stab, then writes the steadied picture. A clip
//! both stabilized and reversed is stabilized first, then that file is
//! reversed.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use openreel_engine::Remade;

use crate::export::{collect_stderr, ScratchDir};
use crate::MediaResult;

mod reverse;
mod stabilize;

/// The quality of the picture written: x264's CRF, finer than export's
/// Higher (16), since export encodes it again.
const CRF: &str = "14";
/// The sound's rate, the mix's.
const RATE: u32 = 48_000;
/// The most threads a decoder and x264 each get. On 16 cores x264's
/// default held 2.5 GB encoding 4K and 8 threads 1.46 GB (11.6 s for 420
/// frames against 9.6 s); the decoder's held 1.13 GB and 8 threads 667 MB,
/// as fast.
const MAX_THREADS: usize = 8;

/// The range of a file whose frames lie on its frame grid that covers
/// `[start, end)`: its first frame's start and its last frame's end. A
/// remade file holds whole frames, so its range is this one.
pub fn frame_range(fps: f64, start: f64, end: f64) -> (f64, f64) {
    if !fps.is_finite() || fps <= 0.0 {
        return (start, end);
    }
    let first = (start * fps + 1e-6).floor().max(0.0);
    let last = (end * fps - 1e-6).ceil().max(first + 1.0);
    (first / fps, last / fps)
}

/// Write what `spec` says to `output`: `[start, end)` of its original
/// (which should be a [`frame_range`]), stabilized and backwards as it
/// says, as a QuickTime file of H.264 picture (BT.709 limited range, 8-bit
/// 4:2:0, as export makes) and 16-bit PCM sound at 48 kHz. The file appears
/// whole or not at all. `progress` hears the share done; setting `cancel`
/// stops the work with an error.
pub fn remake(
    spec: &Remade,
    output: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> MediaResult<()> {
    let limits = Limits {
        chunk_frames: usize::MAX,
        detect_side: stabilize::DETECT_SIDE,
    };
    remake_with(spec, output, progress, cancel, limits)
}

/// Sizes the tests make small, to reach the parts that matter on large
/// files: the most frames one reversed chunk holds, and the longest side
/// motion is measured at.
#[derive(Clone, Copy)]
struct Limits {
    chunk_frames: usize,
    detect_side: u32,
}

fn remake_with(
    spec: &Remade,
    output: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
    limits: Limits,
) -> MediaResult<()> {
    let source = Source::read(&spec.original)?;
    let scratch = ScratchDir::beside(output)?;
    let partial = scratch.0.join("remade.mov");
    let range = (spec.start, spec.end);
    let Limits {
        chunk_frames,
        detect_side,
    } = limits;
    match (spec.stabilize, spec.reverse) {
        (None, false) => return Err("nothing to remake".to_owned()),
        (None, true) => reverse::reverse(
            &source,
            range,
            &scratch.0,
            &partial,
            progress,
            cancel,
            chunk_frames,
        )?,
        (Some(level), false) => stabilize::stabilize(
            &source,
            range,
            level,
            &scratch.0,
            &partial,
            progress,
            cancel,
            detect_side,
        )?,
        (Some(level), true) => {
            // Steadied forwards, as the camera moved, then reversed.
            let steady = scratch.0.join("steady.mov");
            stabilize::stabilize(
                &source,
                range,
                level,
                &scratch.0,
                &steady,
                &mut |share| progress(share * 0.6),
                cancel,
                detect_side,
            )?;
            let steady = Source::read(&steady.to_string_lossy())?;
            reverse::reverse(
                &steady,
                (0.0, spec.end - spec.start),
                &scratch.0,
                &partial,
                &mut |share| progress(0.6 + share * 0.4),
                cancel,
                chunk_frames,
            )?;
        }
    }
    fs::rename(&partial, output).map_err(|e| format!("save {}: {e}", output.display()))?;
    progress(1.0);
    Ok(())
}

/// What a remade file needs to know about the file it reads.
struct Source {
    path: String,
    /// Its picture, cropped to even sides; none for a file of sound only.
    picture: Option<Picture>,
    /// Channels its sound keeps: one for mono, else two. None without sound.
    channels: Option<u16>,
}

#[derive(Clone, Copy)]
struct Picture {
    width: u32,
    height: u32,
    /// Frames per second as a fraction.
    rate: (i32, i32),
}

impl Picture {
    fn fps(&self) -> f64 {
        f64::from(self.rate.0) / f64::from(self.rate.1)
    }

    fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 3 / 2
    }

    /// The frames `[start, end)` holds.
    fn frames(&self, start: f64, end: f64) -> usize {
        ((end - start) * self.fps()).round().max(1.0) as usize
    }
}

impl Source {
    fn read(path: &str) -> MediaResult<Self> {
        use ffmpeg_next::media::Type;
        let info = crate::probe(path)?;
        if info.photo {
            return Err(format!("{path} is a photo"));
        }
        let input = ffmpeg_next::format::input(path).map_err(|e| format!("{path}: {e}"))?;
        let picture = match input.streams().best(Type::Video) {
            Some(video) if info.has_video => {
                let rate = [video.avg_frame_rate(), video.rate()]
                    .into_iter()
                    .find(|rate| rate.numerator() > 0 && rate.denominator() > 0)
                    .map(|rate| (rate.numerator(), rate.denominator()))
                    .ok_or_else(|| format!("{path} has no frame rate"))?;
                let (width, height) = (info.width & !1, info.height & !1);
                if width == 0 || height == 0 {
                    return Err(format!("{path} has no picture"));
                }
                Some(Picture {
                    width,
                    height,
                    rate,
                })
            }
            _ => None,
        };
        let channels = input.streams().best(Type::Audio).map(|audio| {
            ffmpeg_next::codec::context::Context::from_parameters(audio.parameters())
                .ok()
                .and_then(|context| context.decoder().audio().ok())
                .map_or(2, |decoder| if decoder.channels() == 1 { 1 } else { 2 })
        });
        let channels = info.has_audio.then_some(channels).flatten();
        if picture.is_none() && channels.is_none() {
            return Err(format!("{path} has neither picture nor sound"));
        }
        Ok(Self {
            path: path.to_owned(),
            picture,
            channels,
        })
    }

    /// The sound of `[start, end)` as a WAV file in `scratch`, backwards
    /// when `backwards`; none for a file without sound.
    fn sound(
        &self,
        (start, end): (f64, f64),
        backwards: bool,
        scratch: &Path,
    ) -> MediaResult<Option<std::path::PathBuf>> {
        let Some(channels) = self.channels else {
            return Ok(None);
        };
        let raw = scratch.join("sound.raw");
        let wav = scratch.join("sound.wav");
        decode_sound(&self.path, start, end, channels, &raw)?;
        write_wav(&raw, &wav, channels, backwards)?;
        let _ = fs::remove_file(&raw);
        Ok(Some(wav))
    }

    /// Half a frame before `start`, so rounding never loses its first
    /// frame: where a decoder of the picture seeks to.
    fn seek(&self, start: f64) -> String {
        let fps = self.picture.map_or(1.0, |p| p.fps());
        format!("{:.6}", (start - 0.5 / fps).max(0.0))
    }
}

/// An x264 encoder writing `partial` from raw pictures of `picture`'s size
/// and rate on its input, with the WAV `sound` muxed in.
fn encoder(
    picture: Picture,
    sound: Option<&Path>,
    partial: &Path,
) -> MediaResult<(Child, ChildStdin, thread::JoinHandle<String>)> {
    let Picture {
        width,
        height,
        rate: (rate_num, rate_den),
    } = picture;
    let mut encoder = crate::cli::ffmpeg();
    encoder
        .args(["-v", "error", "-nostdin", "-y", "-f", "rawvideo"])
        .args(["-pixel_format", "yuv420p", "-video_size"])
        .arg(format!("{width}x{height}"))
        .arg("-framerate")
        .arg(format!("{rate_num}/{rate_den}"))
        .args(["-colorspace", "bt709", "-color_range", "tv", "-i", "pipe:0"]);
    if let Some(wav) = sound {
        encoder
            .arg("-i")
            .arg(wav)
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "copy"]);
    }
    let mut encoder = encoder
        .args(["-c:v", "libx264", "-threads:v", &threads()])
        // One frame in flight, cut into slices: at 4K, 872 MB against 1.46
        // GB for eight frames at once, 12% slower.
        .args(["-x264-params", "sliced-threads=1"])
        .args(["-preset", "veryfast", "-crf", CRF])
        .args([
            "-g",
            &(picture.fps().round().max(1.0) as u32).to_string(),
            "-f",
            "mov",
        ])
        .arg(partial)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let errors = collect_stderr(encoder.stderr.take());
    let input = encoder.stdin.take().expect("piped encoder input");
    Ok((encoder, input, errors))
}

/// A file of sound alone: the WAV `sound` in a QuickTime file.
fn sound_only(sound: &Path, partial: &Path) -> MediaResult<()> {
    let mut mux = crate::cli::ffmpeg();
    mux.args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(sound)
        .args(["-c:a", "copy", "-f", "mov"])
        .arg(partial);
    run(mux, "writing the sound")
}

/// End `encoder` once `fed` says how feeding it went: an error kills it
/// and names what FFmpeg said last.
fn end_encoder(
    mut encoder: Child,
    errors: thread::JoinHandle<String>,
    fed: MediaResult<()>,
    what: &str,
) -> MediaResult<()> {
    if let Err(error) = fed {
        let _ = encoder.kill();
        let _ = encoder.wait();
        let stderr = errors.join().unwrap_or_default();
        return Err(with_stderr(error, &stderr));
    }
    finish(&mut encoder, errors, what)
}

/// The decoders' and x264's threads: the CPU budget (cgroup quota
/// included), at most [`MAX_THREADS`].
fn threads() -> String {
    thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(MAX_THREADS)
        .to_string()
}

fn cancelled(cancel: &AtomicBool) -> MediaResult<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("cancelled".to_owned())
    } else {
        Ok(())
    }
}

/// The sound of `[start, end)` at 48 kHz, exactly as many samples as the
/// range lasts (silence where the file has none), as raw 16-bit PCM.
fn decode_sound(source: &str, start: f64, end: f64, channels: u16, raw: &Path) -> MediaResult<()> {
    let samples = ((end - start) * f64::from(RATE)).round() as u64;
    let mut decode = crate::cli::ffmpeg();
    decode
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-ss",
            &format!("{start:.6}"),
            "-i",
        ])
        .arg(source)
        .args(["-map", "0:a:0", "-vn", "-sn", "-af"])
        .arg(format!(
            "aresample={RATE},apad=whole_len={samples},atrim=end_sample={samples}"
        ))
        .args([
            "-ac",
            &channels.to_string(),
            "-f",
            "s16le",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(raw);
    run(decode, "decoding the sound")
}

/// Write the 16-bit PCM in `raw` as a WAV file, frame by frame backwards
/// when `backwards`.
fn write_wav(raw: &Path, wav: &Path, channels: u16, backwards: bool) -> MediaResult<()> {
    let fail = |e: io::Error| format!("writing the sound: {e}");
    let mut input = File::open(raw).map_err(fail)?;
    let frame = usize::from(channels) * 2;
    let length = input.metadata().map_err(fail)?.len();
    let length = length - length % frame as u64;
    let mut out = BufWriter::new(File::create(wav).map_err(fail)?);
    out.write_all(&wav_header(channels, length)).map_err(fail)?;
    if !backwards {
        io::copy(&mut input.take(length), &mut out).map_err(fail)?;
        return out.flush().map_err(fail);
    }
    let block = (1 << 16) * frame as u64;
    let mut buffer = vec![0u8; block as usize];
    let mut end = length;
    while end > 0 {
        let begin = end.saturating_sub(block);
        let bytes = &mut buffer[..(end - begin) as usize];
        input.seek(SeekFrom::Start(begin)).map_err(fail)?;
        input.read_exact(bytes).map_err(fail)?;
        for samples in bytes.chunks_exact(frame).rev() {
            out.write_all(samples).map_err(fail)?;
        }
        end = begin;
    }
    out.flush().map_err(fail)
}

/// A canonical 44-byte WAV header for `data` bytes of 16-bit PCM at 48 kHz.
fn wav_header(channels: u16, data: u64) -> [u8; 44] {
    let data = u32::try_from(data).unwrap_or(u32::MAX - 36);
    let block = channels * 2;
    let mut header = [0u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&RATE.to_le_bytes());
    header[28..32].copy_from_slice(&(RATE * u32::from(block)).to_le_bytes());
    header[32..34].copy_from_slice(&block.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data.to_le_bytes());
    header
}

/// Run `command` to its end; an error names `what` and FFmpeg's last words.
fn run(mut command: Command, what: &str) -> MediaResult<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let errors = collect_stderr(child.stderr.take());
    finish(&mut child, errors, what)
}

fn finish(child: &mut Child, errors: thread::JoinHandle<String>, what: &str) -> MediaResult<()> {
    let status = child.wait().map_err(|e| format!("{what}: {e}"))?;
    let stderr = errors.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(with_stderr(format!("{what} failed"), &stderr))
    }
}

fn with_stderr(error: String, stderr: &str) -> String {
    match stderr.lines().rev().find(|line| !line.trim().is_empty()) {
        Some(line) => format!("{error}: {}", line.trim()),
        None => error,
    }
}

#[cfg(test)]
mod tests;

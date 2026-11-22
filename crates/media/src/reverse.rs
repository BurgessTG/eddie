//! A clip played backwards, as CapCut makes one: a range of a file written
//! backwards into a file of its own, which preview, export and pictures
//! then read like any other.
//!
//! One encoder takes the pictures last first. The picture is decoded in
//! chunks of at most [`CHUNK_BYTES`], from the last chunk to the first,
//! each read from the keyframe before it; a chunk decodes while the one
//! after it is written backwards to the encoder, so two chunks are in
//! memory at most. The sound is decoded to a scratch file and read back
//! from its end before the picture starts.
//!
//! Encoding each chunk into a segment of its own and joining them was 5.6
//! times slower: x264 spent about 2 s starting and draining on every
//! 15-frame 4K segment.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::thread;

use crate::export::{collect_stderr, standard_picture, ScratchDir};
use crate::MediaResult;

/// Decoded pictures one chunk holds at most; two are in memory at once.
const CHUNK_BYTES: usize = 192 << 20;
/// The quality of the reversed picture: x264's CRF, finer than export's
/// Higher (16), since export encodes it again.
const CRF: &str = "14";
/// The sound's rate, the mix's.
const RATE: u32 = 48_000;
/// The most threads the decoder and x264 each get. On 16 cores x264's
/// default held 2.5 GB encoding 4K and 8 threads 1.46 GB (11.6 s for 420
/// frames against 9.6 s); the decoder's held 1.13 GB and 8 threads 667 MB,
/// as fast.
const MAX_THREADS: usize = 8;

/// The range of a file whose frames lie on its frame grid that covers
/// `[start, end)`: its first frame's start and its last frame's end. A
/// reversed file holds whole frames, so its range is this one.
pub fn frame_range(fps: f64, start: f64, end: f64) -> (f64, f64) {
    if !fps.is_finite() || fps <= 0.0 {
        return (start, end);
    }
    let first = (start * fps + 1e-6).floor().max(0.0);
    let last = (end * fps - 1e-6).ceil().max(first + 1.0);
    (first / fps, last / fps)
}

/// Write `[start, end)` of `source` backwards to `output`, a QuickTime
/// file of H.264 picture (BT.709 limited range, 8-bit 4:2:0, as export
/// makes) and 16-bit PCM sound at 48 kHz. `start` and `end` should be a
/// [`frame_range`]. The file appears whole or not at all. `progress` hears
/// the share done; setting `cancel` stops the work with an error.
pub fn reverse_range(
    source: &str,
    start: f64,
    end: f64,
    output: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> MediaResult<()> {
    reverse(source, (start, end), output, progress, cancel, usize::MAX)
}

/// How the picture is read: its size after cropping to even sides, its
/// rate, and how many frames from `start` the range holds.
struct Picture<'a> {
    source: &'a str,
    start: f64,
    fps: f64,
    size: (u32, u32),
    frames: usize,
}

impl Picture<'_> {
    fn frame_bytes(&self) -> usize {
        self.size.0 as usize * self.size.1 as usize * 3 / 2
    }

    /// Frames `[first, first + count)` of the range, from the keyframe
    /// before them, into buffers taken from `spare` where it has some.
    fn chunk(
        &self,
        first: usize,
        count: usize,
        spare: &Receiver<Vec<u8>>,
    ) -> MediaResult<Vec<Vec<u8>>> {
        let (width, height) = self.size;
        // Half a frame early, so rounding never loses the first; held at
        // the end if the file runs out first.
        let at = self.start + (first as f64 - 0.5) / self.fps;
        let mut decoder = crate::cli::ffmpeg()
            .args(["-v", "error", "-nostdin", "-threads", &threads()])
            .args(["-ss", &format!("{:.6}", at.max(0.0))])
            .arg("-i")
            .arg(self.source)
            .args(["-map", "0:v:0", "-an", "-sn", "-vf"])
            .arg(format!(
                "crop={width}:{height}:0:0,{},tpad=stop_mode=clone:stop=-1",
                standard_picture(self.source)
            ))
            .args(["-frames:v", &count.to_string()])
            .args(["-fps_mode", "passthrough", "-f", "rawvideo", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
        let errors = collect_stderr(decoder.stderr.take());
        let mut out = decoder.stdout.take().expect("piped decoder output");
        let mut pictures = Vec::with_capacity(count);
        let mut read = Ok(());
        for _ in 0..count {
            let mut buffer = spare
                .try_recv()
                .unwrap_or_else(|_| vec![0; self.frame_bytes()]);
            read = out.read_exact(&mut buffer);
            if read.is_err() {
                break;
            }
            pictures.push(buffer);
        }
        drop(out);
        if let Err(error) = read {
            let _ = decoder.kill();
            let _ = decoder.wait();
            let stderr = errors.join().unwrap_or_default();
            return Err(with_stderr(
                format!("decoding the picture: {error}"),
                &stderr,
            ));
        }
        finish(&mut decoder, errors, "decoding the picture")?;
        Ok(pictures)
    }
}

/// [`reverse_range`] with chunks of at most `max_chunk` frames.
fn reverse(
    source: &str,
    (start, end): (f64, f64),
    output: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
    max_chunk: usize,
) -> MediaResult<()> {
    let stream = Streams::read(source)?;
    let (rate_num, rate_den) = stream.rate;
    let fps = f64::from(rate_num) / f64::from(rate_den);
    let (width, height) = (stream.width & !1, stream.height & !1);
    if width == 0 || height == 0 {
        return Err(format!("{source} has no picture to reverse"));
    }
    let picture = Picture {
        source,
        start,
        fps,
        size: (width, height),
        frames: ((end - start) * fps).round().max(1.0) as usize,
    };
    let scratch = ScratchDir::beside(output)?;

    // The sound first, so the encoder takes it in as it writes.
    let sound = match stream.channels {
        Some(channels) => {
            let raw = scratch.0.join("sound.raw");
            let wav = scratch.0.join("sound.wav");
            decode_sound(source, start, end, channels, &raw)?;
            reverse_pcm(&raw, &wav, channels)?;
            let _ = fs::remove_file(&raw);
            Some(wav)
        }
        None => None,
    };
    progress(0.02);

    let partial = scratch.0.join("reversed.mov");
    let mut encoder = crate::cli::ffmpeg();
    encoder
        .args(["-v", "error", "-nostdin", "-y", "-f", "rawvideo"])
        .args(["-pixel_format", "yuv420p", "-video_size"])
        .arg(format!("{width}x{height}"))
        .arg("-framerate")
        .arg(format!("{rate_num}/{rate_den}"))
        .args(["-colorspace", "bt709", "-color_range", "tv", "-i", "pipe:0"]);
    if let Some(wav) = &sound {
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
            &(fps.round().max(1.0) as u32).to_string(),
            "-f",
            "mov",
        ])
        .arg(&partial)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let encoder_errors = collect_stderr(encoder.stderr.take());
    let input = encoder.stdin.take().expect("piped encoder input");

    let chunk = (CHUNK_BYTES / picture.frame_bytes())
        .min(max_chunk)
        .clamp(1, picture.frames);
    let (full_tx, full_rx) = sync_channel(0);
    let (spare_tx, spare_rx) = channel();
    let fed = thread::scope(|scope| {
        let picture = &picture;
        scope.spawn(move || decode_backwards(picture, chunk, &full_tx, &spare_rx, cancel));
        // Owns the receiving end, so a decoder still sending once this
        // stops is told, and the scope can end.
        feed(input, full_rx, spare_tx, picture.frames, progress, cancel)
    });
    if let Err(error) = fed {
        let _ = encoder.kill();
        let _ = encoder.wait();
        let stderr = encoder_errors.join().unwrap_or_default();
        return Err(with_stderr(error, &stderr));
    }
    finish(
        &mut encoder,
        encoder_errors,
        "encoding the reversed picture",
    )?;
    fs::rename(&partial, output).map_err(|e| format!("save {}: {e}", output.display()))?;
    progress(1.0);
    Ok(())
}

/// The decoder's and x264's threads: the CPU budget (cgroup quota
/// included), at most [`MAX_THREADS`].
fn threads() -> String {
    thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(MAX_THREADS)
        .to_string()
}

/// On its own thread: decode the range's chunks from the last to the first
/// and send each on, until done, refused or cancelled.
fn decode_backwards(
    picture: &Picture,
    chunk: usize,
    full: &SyncSender<MediaResult<Vec<Vec<u8>>>>,
    spare: &Receiver<Vec<u8>>,
    cancel: &AtomicBool,
) {
    let chunks = picture.frames.div_ceil(chunk);
    for index in (0..chunks).rev() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let first = index * chunk;
        let decoded = picture.chunk(first, chunk.min(picture.frames - first), spare);
        let failed = decoded.is_err();
        if full.send(decoded).is_err() || failed {
            return;
        }
    }
}

/// Write each chunk's pictures to the encoder last first, handing the
/// buffers back to the decoder.
fn feed(
    mut input: ChildStdin,
    full: Receiver<MediaResult<Vec<Vec<u8>>>>,
    spare: Sender<Vec<u8>>,
    frames: usize,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
) -> MediaResult<()> {
    let mut written = 0;
    while written < frames {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_owned());
        }
        let pictures = full
            .recv()
            .map_err(|_| "the picture decoder stopped".to_owned())??;
        for picture in pictures.into_iter().rev() {
            input
                .write_all(&picture)
                .map_err(|e| format!("encoding the reversed picture: {e}"))?;
            written += 1;
            let _ = spare.send(picture);
        }
        progress(0.02 + 0.97 * written as f64 / frames as f64);
    }
    Ok(())
}

/// What the reversed file needs to know about a source's streams.
struct Streams {
    width: u32,
    height: u32,
    /// Frames per second as a fraction.
    rate: (i32, i32),
    /// Channels its sound keeps: one for mono, else two. None without sound.
    channels: Option<u16>,
}

impl Streams {
    fn read(source: &str) -> MediaResult<Self> {
        use ffmpeg_next::media::Type;
        let info = crate::probe(source)?;
        if !info.has_video || info.photo {
            return Err(format!("{source} has no moving picture to reverse"));
        }
        let input = ffmpeg_next::format::input(source).map_err(|e| format!("{source}: {e}"))?;
        let video = input
            .streams()
            .best(Type::Video)
            .ok_or_else(|| format!("{source} has no picture"))?;
        let rate = [video.avg_frame_rate(), video.rate()]
            .into_iter()
            .find(|rate| rate.numerator() > 0 && rate.denominator() > 0)
            .map(|rate| (rate.numerator(), rate.denominator()))
            .ok_or_else(|| format!("{source} has no frame rate"))?;
        let channels = input.streams().best(Type::Audio).map(|audio| {
            ffmpeg_next::codec::context::Context::from_parameters(audio.parameters())
                .ok()
                .and_then(|context| context.decoder().audio().ok())
                .map_or(2, |decoder| if decoder.channels() == 1 { 1 } else { 2 })
        });
        Ok(Self {
            width: info.width,
            height: info.height,
            rate,
            channels: info.has_audio.then_some(channels).flatten(),
        })
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

/// Write the 16-bit PCM in `raw` backwards, frame by frame, as a WAV file.
fn reverse_pcm(raw: &Path, wav: &Path, channels: u16) -> MediaResult<()> {
    let fail = |e: io::Error| format!("reversing the sound: {e}");
    let mut input = File::open(raw).map_err(fail)?;
    let frame = usize::from(channels) * 2;
    let length = input.metadata().map_err(fail)?.len();
    let length = length - length % frame as u64;
    let mut out = BufWriter::new(File::create(wav).map_err(fail)?);
    out.write_all(&wav_header(channels, length)).map_err(fail)?;
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

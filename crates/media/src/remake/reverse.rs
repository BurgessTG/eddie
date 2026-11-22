//! A range written backwards. One encoder takes the pictures last first.
//! The picture is decoded in chunks of at most [`CHUNK_BYTES`], from the
//! last chunk to the first, each read from the keyframe before it; a chunk
//! decodes while the one after it is written backwards to the encoder, so
//! two chunks are in memory at most. The sound is decoded to a scratch
//! file and read back from its end before the picture starts.
//!
//! Encoding each chunk into a segment of its own and joining them was 5.6
//! times slower: x264 spent about 2 s starting and draining on every
//! 15-frame 4K segment.

use std::io::Read;
use std::path::Path;
use std::process::{ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::thread;

use super::{
    collect_stderr, encoder, end_encoder, finish, sound_only, threads, with_stderr, Picture, Source,
};
use crate::export::standard_picture;
use crate::MediaResult;

/// Decoded pictures one chunk holds at most; two are in memory at once.
const CHUNK_BYTES: usize = 192 << 20;

/// Write `[start, end)` of `source` backwards to `partial`, in chunks of
/// at most `max_chunk` frames.
pub(super) fn reverse(
    source: &Source,
    (start, end): (f64, f64),
    scratch: &Path,
    partial: &Path,
    progress: &mut dyn FnMut(f64),
    cancel: &AtomicBool,
    max_chunk: usize,
) -> MediaResult<()> {
    // The sound first, so the encoder takes it in as it writes.
    let sound = source.sound((start, end), true, scratch)?;
    progress(0.02);
    let Some(picture) = source.picture else {
        let sound = sound.ok_or_else(|| format!("{} has nothing to reverse", source.path))?;
        return sound_only(&sound, partial);
    };
    let chunks = Chunks {
        source: &source.path,
        start,
        picture,
        frames: picture.frames(start, end),
    };
    let (encoder, input, errors) = encoder(picture, sound.as_deref(), partial)?;
    let chunk = (CHUNK_BYTES / picture.frame_bytes())
        .min(max_chunk)
        .clamp(1, chunks.frames);
    let (full_tx, full_rx) = sync_channel(0);
    let (spare_tx, spare_rx) = channel();
    let fed = thread::scope(|scope| {
        let chunks = &chunks;
        scope.spawn(move || decode_backwards(chunks, chunk, &full_tx, &spare_rx, cancel));
        // Owns the receiving end, so a decoder still sending once this
        // stops is told, and the scope can end.
        feed(input, full_rx, spare_tx, chunks.frames, progress, cancel)
    });
    end_encoder(encoder, errors, fed, "encoding the reversed picture")
}

/// How the picture is read: from which file and time, and how many frames
/// from there the range holds.
struct Chunks<'a> {
    source: &'a str,
    start: f64,
    picture: Picture,
    frames: usize,
}

impl Chunks<'_> {
    /// Frames `[first, first + count)` of the range, from the keyframe
    /// before them, into buffers taken from `spare` where it has some.
    fn chunk(
        &self,
        first: usize,
        count: usize,
        spare: &Receiver<Vec<u8>>,
    ) -> MediaResult<Vec<Vec<u8>>> {
        let Picture { width, height, .. } = self.picture;
        // Half a frame early, so rounding never loses the first; held at
        // the end if the file runs out first.
        let at = self.start + (first as f64 - 0.5) / self.picture.fps();
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
                .unwrap_or_else(|_| vec![0; self.picture.frame_bytes()]);
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

/// On its own thread: decode the range's chunks from the last to the first
/// and send each on, until done, refused or cancelled.
fn decode_backwards(
    chunks: &Chunks,
    chunk: usize,
    full: &SyncSender<MediaResult<Vec<Vec<u8>>>>,
    spare: &Receiver<Vec<u8>>,
    cancel: &AtomicBool,
) {
    let count = chunks.frames.div_ceil(chunk);
    for index in (0..count).rev() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let first = index * chunk;
        let decoded = chunks.chunk(first, chunk.min(chunks.frames - first), spare);
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
    use std::io::Write;
    let mut written = 0;
    while written < frames {
        super::cancelled(cancel)?;
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

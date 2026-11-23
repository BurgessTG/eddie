//! Paused stills and filmstrips through the in-process decoder.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use ffmpeg_next as ffmpeg;

use super::{VideoDecoder, RECENT_PICTURES};
use crate::{MediaResult, RawFrame};

/// Filmstrip stills at `times` (ascending), keyed by each still's PTS.
/// A sample within `tolerance` after its keyframe shows that keyframe, from
/// a slice-threaded decoder that decodes keyframes only; a sample deep
/// inside a long GOP (or without an index) decodes forward to its exact
/// picture on a second, frame-threaded decoder. A screen recording with a
/// keyframe every 8 s decodes every picture that way: on one core (its
/// encoder writes one slice a picture) 158 s of 1080p took 30.5 s.
pub fn filmstrip_stills(
    path: &str,
    times: &[f64],
    target_w: u32,
    tolerance: f64,
) -> MediaResult<Vec<(f64, RawFrame)>> {
    use ffmpeg::codec::threading::Type;
    let mut keys = VideoDecoder::open_threaded(path, target_w, true, Type::Slice, 0, false)?;
    if keys.photo {
        // Its one picture decoded as it opened.
        return Ok(next_still(&mut keys)?.into_iter().collect());
    }
    keys.decoder.skip_frame(ffmpeg::Discard::NonKey);
    let (_, width, height) = keys.scaler.source;
    let pixels = (u64::from(width) * u64::from(height)).max(1);
    let threads =
        (FORWARD_THREADS as u64 * 1920 * 1080 / pixels).clamp(2, FORWARD_THREADS as u64) as usize;
    let threads = threads.min(std::thread::available_parallelism().map_or(4, |n| n.get()));
    let mut forward: Option<VideoDecoder> = None;
    // The last picture the forward decoder returned.
    let mut position: Option<f64> = None;
    let mut stills: Vec<(f64, RawFrame)> = Vec::new();
    for &time in times {
        let key = keys.keyframe_before(time);
        let shown = if key.is_some_and(|key| time - key <= tolerance) {
            let last = stills.last().map(|(pts, _)| *pts);
            if matches!((last, key), (Some(at), Some(key)) if key <= at) {
                continue; // The previous still is this time's keyframe or later.
            }
            // The backward seek lands on the keyframe at or before `time`;
            // take it without decoding the next one to bound its interval.
            keys.seek(time)?;
            keys.seek_target = None;
            next_still(&mut keys)?
        } else {
            let decoder = match &mut forward {
                Some(decoder) => decoder,
                None => forward.insert(VideoDecoder::open_threaded(
                    path,
                    target_w,
                    true,
                    Type::Frame,
                    threads,
                    false,
                )?),
            };
            let same_gop = matches!((position, key), (Some(at), Some(key)) if key <= at);
            if same_gop || (key.is_none() && position.is_some()) {
                decoder.skip_to(time);
            } else {
                decoder.seek(time)?;
            }
            let still = next_still(decoder)?;
            position = still.as_ref().map(|(pts, _)| *pts);
            still
        };
        let Some((pts, frame)) = shown else {
            break;
        };
        if stills.last().is_some_and(|(last, _)| *last >= pts) {
            continue;
        }
        stills.push((pts, frame));
    }
    Ok(stills)
}

/// The frame threads a filmstrip's forward decoder gets at 1080p. Each
/// thread holds pictures of its own and up to four files import at once,
/// so larger pictures get fewer, down to 2 at 4K: with 8 threads the
/// submission's import peaked at 931 MB, against 595 MB on one.
const FORWARD_THREADS: usize = 8;

/// The decoder's next picture at filmstrip size, and its time.
fn next_still(decoder: &mut VideoDecoder) -> MediaResult<Option<(f64, RawFrame)>> {
    let Some(pts) = decoder.next_decoded()? else {
        return Ok(None);
    };
    Ok(Some((pts, decoder.scale_decoded_into(Vec::new())?)))
}

/// One-shot still frame through the in-process decoder.
pub fn frame_at(path: &str, time: f64, target_w: u32, fast: bool) -> MediaResult<RawFrame> {
    VideoDecoder::open(path, target_w, fast)?.frame_at(time)
}

struct StillRequest {
    id: u64,
    path: String,
    src_w: u32,
    src_h: u32,
    time: f64,
    target_w: u32,
    fast: bool,
}

/// Persistent scrub decoder on a worker thread.
///
/// Reuses one `VideoDecoder` per file across widths, so stepping and short
/// forward moves decode on and stepping back reads recent pictures instead
/// of seeking. Coalesces queued requests to the latest one, so fast
/// scrubbing never builds a backlog of stale decodes.
pub struct StillDecoder {
    tx: Sender<StillRequest>,
}

impl StillDecoder {
    pub fn spawn(
        wake: impl Fn() + Send + 'static,
    ) -> (Self, Receiver<(u64, MediaResult<RawFrame>)>) {
        let (request_tx, request_rx) = channel::<StillRequest>();
        let (result_tx, result_rx) = channel::<(u64, MediaResult<RawFrame>)>();
        thread::spawn(move || {
            let mut current: Option<(String, VideoDecoder)> = None;
            while let Ok(mut request) = request_rx.recv() {
                while let Ok(latest) = request_rx.try_recv() {
                    request = latest;
                }
                if current
                    .as_ref()
                    .is_none_or(|(path, _)| *path != request.path)
                {
                    current = VideoDecoder::open(&request.path, request.target_w, request.fast)
                        .ok()
                        .map(|mut decoder| {
                            decoder.recent_limit = RECENT_PICTURES;
                            (request.path.clone(), decoder)
                        });
                }
                let mut result = match current.as_mut() {
                    Some((_, decoder)) => decoder
                        .set_output(request.target_w, request.fast)
                        .and_then(|()| decoder.still_at(request.time)),
                    None => Err("decoder unavailable".to_owned()),
                };
                if result.is_err() {
                    // Reopen once / fall back to the CLI path in lib.
                    current = None;
                    result = crate::frame_at(
                        &request.path,
                        request.src_w,
                        request.src_h,
                        request.time,
                        request.target_w,
                        request.fast,
                    );
                }
                if result_tx.send((request.id, result)).is_err() {
                    break;
                }
                wake();
            }
        });
        (Self { tx: request_tx }, result_rx)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request(
        &self,
        id: u64,
        path: String,
        src_w: u32,
        src_h: u32,
        time: f64,
        target_w: u32,
        fast: bool,
    ) {
        let _ = self.tx.send(StillRequest {
            id,
            path,
            src_w,
            src_h,
            time,
            target_w,
            fast,
        });
    }
}

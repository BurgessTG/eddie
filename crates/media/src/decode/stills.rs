//! Paused stills and filmstrips through the in-process decoder.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use ffmpeg_next as ffmpeg;

use super::{VideoDecoder, RECENT_PICTURES};
use crate::{MediaResult, RawFrame};

/// Filmstrip stills at `times` (ascending), keyed by each still's PTS.
/// A sample within `tolerance` after its keyframe shows that keyframe,
/// decoding keyframes only; a sample deep inside a long GOP (or without an
/// index) decodes forward to its exact picture instead.
pub fn filmstrip_stills(
    path: &str,
    times: &[f64],
    target_w: u32,
    tolerance: f64,
) -> MediaResult<Vec<(f64, RawFrame)>> {
    let mut decoder = VideoDecoder::open_threaded(
        path,
        target_w,
        true,
        ffmpeg::codec::threading::Type::Slice,
        false,
    )?;
    let mut stills: Vec<(f64, RawFrame)> = Vec::new();
    let mut position: Option<f64> = None;
    // The last still was read keyframes-only. With B-frames the decoder
    // holds a keyframe until later keyframes arrive, so the demuxer is GOPs
    // past `position` and decoding forward from here would skip the
    // pictures in between.
    let mut read_ahead = false;
    for &time in times {
        let key = decoder.keyframe_before(time);
        let same_gop = matches!((position, key), (Some(at), Some(key)) if key <= at);
        if key.is_some_and(|key| time - key <= tolerance) {
            if same_gop {
                continue; // The previous still is this time's keyframe.
            }
            decoder.decoder.skip_frame(ffmpeg::Discard::NonKey);
            // The backward seek lands on the keyframe at or before `time`;
            // take it without decoding the next one to bound its interval.
            decoder.seek(time)?;
            decoder.seek_target = None;
            read_ahead = true;
        } else {
            decoder.decoder.skip_frame(ffmpeg::Discard::Default);
            if !read_ahead && (same_gop || (key.is_none() && position.is_some())) {
                decoder.skip_to(time);
            } else {
                decoder.seek(time)?;
            }
            read_ahead = false;
        }
        let Some(pts) = decoder.next_decoded()? else {
            break;
        };
        position = Some(pts);
        if stills.last().is_some_and(|(last, _)| *last >= pts) {
            continue;
        }
        let frame = decoder.scale_decoded_into(Vec::new())?;
        stills.push((pts, frame));
    }
    Ok(stills)
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

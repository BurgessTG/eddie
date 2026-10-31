//! In-process video decoding via ffmpeg-next (libavcodec).
//!
//! Replaces process-per-frame scrubbing and pipe streaming: one decoder
//! seeks and yields scaled RGBA frames, with libavcodec frame threading.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Once;
use std::thread;

use ffmpeg::Rescale;
use ffmpeg_next as ffmpeg;

use crate::{scaled_height, MediaResult, RawFrame};

static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
        let _ = ffmpeg::init();
    });
}

fn err(context: &str, error: ffmpeg::Error) -> String {
    format!("{context}: {error}")
}

pub struct VideoDecoder {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    scaler: ffmpeg::software::scaling::Context,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    source_origin: i64,
    target_w: u32,
    target_h: u32,
    /// Clockwise quarter turns from the stream's display matrix.
    turns: u8,
    decoded: ffmpeg::frame::Video,
    lookahead: ffmpeg::frame::Video,
    lookahead_pts: Option<f64>,
    seek_target: Option<f64>,
    scaled: ffmpeg::frame::Video,
    eof_sent: bool,
}

impl VideoDecoder {
    pub fn open(path: &str, target_w: u32, fast: bool) -> MediaResult<Self> {
        ensure_init();
        let input = ffmpeg::format::input(path).map_err(|e| err("open input", e))?;
        let (stream_index, time_base, stream_start, parameters, turns) = {
            let stream = input
                .streams()
                .best(ffmpeg::media::Type::Video)
                .ok_or_else(|| "no video stream".to_owned())?;
            let turns = stream
                .side_data()
                .find(|data| {
                    data.kind() == ffmpeg::codec::packet::side_data::Type::DisplayMatrix
                        && data.data().len() >= 36
                })
                .map_or(0, |data| {
                    // SAFETY: a display matrix is nine i32 values in an
                    // av_malloc-aligned side-data buffer owned by the stream.
                    let degrees = unsafe {
                        ffmpeg::ffi::av_display_rotation_get(data.data().as_ptr().cast())
                    };
                    clockwise_turns(degrees)
                });
            (
                stream.index(),
                stream.time_base(),
                stream.start_time(),
                stream.parameters(),
                turns,
            )
        };
        // Match input-relative source times, retaining any video offset from
        // the asset's other streams. ffmpeg-next has no format start accessor.
        let format_start = unsafe { (*input.as_ptr()).start_time };
        let source_origin = if format_start != ffmpeg::ffi::AV_NOPTS_VALUE {
            format_start.rescale(ffmpeg::rescale::TIME_BASE, time_base)
        } else if stream_start != ffmpeg::ffi::AV_NOPTS_VALUE {
            stream_start
        } else {
            0
        };

        let mut context = ffmpeg::codec::context::Context::from_parameters(parameters)
            .map_err(|e| err("decoder parameters", e))?;
        context.set_threading(ffmpeg::codec::threading::Config {
            kind: ffmpeg::codec::threading::Type::Frame,
            count: 0, // libavcodec chooses based on CPU cores.
        });
        let decoder = context
            .decoder()
            .video()
            .map_err(|e| err("open video decoder", e))?;

        // target_w/target_h are display dimensions. A quarter-turn scales to
        // the transposed size, then rotates while copying out.
        let quarter = turns % 2 == 1;
        let target_h = if quarter {
            scaled_height(decoder.height(), decoder.width(), target_w)
        } else {
            scaled_height(decoder.width(), decoder.height(), target_w)
        };
        let (scaled_w, scaled_h) = if quarter {
            (target_h, target_w)
        } else {
            (target_w, target_h)
        };
        let flags = if fast {
            ffmpeg::software::scaling::Flags::FAST_BILINEAR
        } else {
            ffmpeg::software::scaling::Flags::BICUBIC
        };
        let scaler = ffmpeg::software::scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGBA,
            scaled_w,
            scaled_h,
            flags,
        )
        .map_err(|e| err("create scaler", e))?;

        Ok(Self {
            input,
            decoder,
            scaler,
            stream_index,
            time_base,
            source_origin,
            target_w,
            target_h,
            turns,
            decoded: ffmpeg::frame::Video::empty(),
            lookahead: ffmpeg::frame::Video::empty(),
            lookahead_pts: None,
            seek_target: None,
            scaled: ffmpeg::frame::Video::empty(),
            eof_sent: false,
        })
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.target_w, self.target_h)
    }

    /// Seek in source-relative seconds. The next frame covers `time`, not
    /// the demuxer's keyframe preroll. Selection happens on the next decode.
    pub fn seek(&mut self, time: f64) -> MediaResult<()> {
        let time = time.max(0.0);
        let ts = self
            .source_origin
            .rescale(self.time_base, ffmpeg::rescale::TIME_BASE)
            .saturating_add((time * ffmpeg::ffi::AV_TIME_BASE as f64) as i64);
        // The wrapper passes the range end directly as the inclusive max_ts.
        // Never seek to a future keyframe: we need the interval containing t.
        self.input.seek(ts, ..ts).map_err(|e| err("seek", e))?;
        self.decoder.flush();
        self.eof_sent = false;
        self.lookahead_pts = None;
        self.seek_target = Some(time);
        Ok(())
    }

    /// Select the picture covering `time` by decoding forward from the current
    /// position, without a demuxer seek. Only correct when `time` is at or
    /// after every picture already returned; the next frame is returned as-is
    /// if it already starts after `time`.
    pub(crate) fn skip_to(&mut self, time: f64) {
        self.seek_target = Some(time.max(0.0));
    }

    /// Source time of the keyframe that a seek to `time` would decode from,
    /// when the container has an index (MP4/MOV do; raw streams may not).
    pub(crate) fn keyframe_before(&self, time: f64) -> Option<f64> {
        let ts = ((time.max(0.0) * ffmpeg::ffi::AV_TIME_BASE as f64) as i64)
            .rescale(ffmpeg::rescale::TIME_BASE, self.time_base)
            .saturating_add(self.source_origin);
        // SAFETY: the stream belongs to the open input owned by `self`, and the
        // returned entry is only read before any further demuxer call.
        let entry = unsafe {
            let stream = *(*self.input.as_ptr()).streams.add(self.stream_index);
            let entry = ffmpeg::ffi::avformat_index_get_entry_from_timestamp(
                stream,
                ts,
                ffmpeg::ffi::AVSEEK_FLAG_BACKWARD as i32,
            );
            (!entry.is_null()).then(|| (*entry).timestamp)?
        };
        Some(
            entry.saturating_sub(self.source_origin) as f64 * self.time_base.numerator() as f64
                / self.time_base.denominator() as f64,
        )
    }

    fn frame_seconds(&self, frame: &ffmpeg::frame::Video) -> f64 {
        frame
            .timestamp()
            .map(|ts| {
                // Subtract in ticks and divide last, avoiding cancellation at
                // nonzero origins and premature rounding of the time base.
                ts.saturating_sub(self.source_origin) as f64 * self.time_base.numerator() as f64
                    / self.time_base.denominator() as f64
            })
            .unwrap_or(0.0)
    }

    /// Decode one frame, including delayed frames after send_eof. Only a
    /// receive-side EOF means the decoder has actually finished draining.
    fn receive_decoded(&mut self) -> MediaResult<Option<f64>> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => return Ok(Some(self.frame_seconds(&self.decoded))),
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(ffmpeg::Error::Other { errno })
                    if errno == ffmpeg::error::EAGAIN && !self.eof_sent => {}
                Err(e) => return Err(err("receive frame", e)),
            }
            // A receive-side EAGAIN guarantees that sending can make progress.
            // Read directly so a demux error cannot masquerade as clean EOF.
            loop {
                let mut packet = ffmpeg::Packet::empty();
                match packet.read(&mut self.input) {
                    Ok(()) if packet.stream() == self.stream_index => {
                        self.decoder
                            .send_packet(&packet)
                            .map_err(|e| err("send packet", e))?;
                        break;
                    }
                    Ok(()) => continue,
                    Err(ffmpeg::Error::Eof) => {
                        self.decoder.send_eof().map_err(|e| err("send EOF", e))?;
                        self.eof_sent = true;
                        break;
                    }
                    Err(e) => return Err(err("read packet", e)),
                }
            }
        }
    }

    fn next_decoded(&mut self) -> MediaResult<Option<f64>> {
        let seconds = if let Some(pts) = self.lookahead_pts.take() {
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            Some(pts)
        } else {
            self.receive_decoded()?
        };
        let Some(target) = self.seek_target.take() else {
            return Ok(seconds);
        };
        let Some(mut seconds) = seconds else {
            return Ok(None);
        };

        loop {
            // Keep the candidate while decoding its interval's end. Swapping
            // two reusable AVFrames holds references, never copies pixels.
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            let next = self.receive_decoded()?;
            if let Some(pts) = next.filter(|pts| *pts <= target) {
                seconds = pts;
                continue;
            }
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            self.lookahead_pts = next;
            // [seconds, next): equality selected the new frame above. At
            // actual EOF (or before the first PTS), clamp to the nearest frame.
            return Ok(Some(seconds));
        }
    }

    fn scale_decoded_into(&mut self, mut buffer: Vec<u8>) -> MediaResult<RawFrame> {
        self.scaler
            .run(&self.decoded, &mut self.scaled)
            .map_err(|e| err("scale frame", e))?;
        let stride = self.scaled.stride(0);
        let width = self.target_w as usize;
        let height = self.target_h as usize;
        buffer.resize(width * height * 4, 0);
        let data = self.scaled.data(0);
        // Output pixel (x, y) reads this scaled pixel. The scaled picture is
        // width×height, or height×width (transposed) for a quarter turn.
        let source = |x: usize, y: usize| match self.turns {
            1 => (y, width - 1 - x),
            2 => (width - 1 - x, height - 1 - y),
            3 => (height - 1 - y, x),
            _ => (x, y),
        };
        if self.turns == 0 {
            for row in 0..height {
                let src = row * stride;
                let dst = row * width * 4;
                buffer[dst..dst + width * 4].copy_from_slice(&data[src..src + width * 4]);
            }
        } else {
            for y in 0..height {
                for x in 0..width {
                    let (sx, sy) = source(x, y);
                    let src = sy * stride + sx * 4;
                    let dst = (y * width + x) * 4;
                    buffer[dst..dst + 4].copy_from_slice(&data[src..src + 4]);
                }
            }
        }
        Ok(RawFrame {
            width: self.target_w,
            height: self.target_h,
            rgba: buffer,
        })
    }

    /// Next streamed frame plus its source-relative presentation time.
    pub fn next_frame(&mut self, buffer: Vec<u8>) -> Option<(f64, RawFrame)> {
        self.try_next_frame(buffer).ok().flatten()
    }

    pub(crate) fn try_next_frame(
        &mut self,
        buffer: Vec<u8>,
    ) -> MediaResult<Option<(f64, RawFrame)>> {
        let Some(seconds) = self.next_decoded()? else {
            return Ok(None);
        };
        let frame = self.scale_decoded_into(buffer)?;
        Ok(Some((seconds, frame)))
    }

    /// Seek and return the frame covering `time` in [PTS, next PTS).
    /// Before/after the source, return its first/last decoded frame.
    pub fn frame_at(&mut self, time: f64) -> MediaResult<RawFrame> {
        self.frame_at_into(time, Vec::new()).map(|(_, frame)| frame)
    }

    pub(crate) fn frame_at_into(
        &mut self,
        time: f64,
        buffer: Vec<u8>,
    ) -> MediaResult<(f64, RawFrame)> {
        self.seek(time)?;
        let pts = self
            .next_decoded()?
            .ok_or_else(|| "no frame decoded at requested time".to_owned())?;
        self.scale_decoded_into(buffer).map(|frame| (pts, frame))
    }
}

/// Clockwise quarter turns that display a stream, from a display-matrix
/// rotation in degrees (counterclockwise, as `av_display_rotation_get` and
/// ffprobe report it). Matches the FFmpeg CLI's autorotate.
pub(crate) fn clockwise_turns(degrees: f64) -> u8 {
    if degrees.is_finite() {
        (-degrees / 90.0).round().rem_euclid(4.0) as u8
    } else {
        0
    }
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
/// Reuses one `VideoDecoder` across requests for the same file/width and
/// coalesces queued requests to the latest one, so fast scrubbing never
/// builds a backlog of stale decodes.
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
            let mut current: Option<((String, u32, bool), VideoDecoder)> = None;
            while let Ok(mut request) = request_rx.recv() {
                while let Ok(latest) = request_rx.try_recv() {
                    request = latest;
                }
                let key = (request.path.clone(), request.target_w, request.fast);
                if current
                    .as_ref()
                    .map(|(current_key, _)| current_key != &key)
                    .unwrap_or(true)
                {
                    current = VideoDecoder::open(&request.path, request.target_w, request.fast)
                        .ok()
                        .map(|decoder| (key.clone(), decoder));
                }
                let mut result = match current.as_mut() {
                    Some((_, decoder)) => decoder.frame_at(request.time),
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

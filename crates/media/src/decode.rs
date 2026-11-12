//! In-process video decoding via ffmpeg-next (libavcodec).
//!
//! Replaces process-per-frame scrubbing and pipe streaming: one decoder
//! seeks and yields scaled RGBA frames, with libavcodec frame threading.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Once;

use ffmpeg::Rescale;
use ffmpeg_next as ffmpeg;

use crate::{MediaResult, RawFrame};

mod hardware;
mod scaler;
mod stills;

use hardware::{attach, download, on_gpu, sync_surface, HARDWARE_DECODE, INJECT_FAULT};
pub(crate) use hardware::{hardware_broken, hardware_failed};
pub use hardware::{
    inject_hardware_fault, set_hardware_decode, take_hardware_failure, warm_hardware_decode,
};
use scaler::Scaler;
pub(crate) use stills::frame_at;
pub use stills::{filmstrip_stills, StillDecoder};

/// Damaged packets in a row a decoder skips before it reports the file as
/// unreadable: about ten seconds of pictures at 30 a second, six or seven
/// of sound.
pub(crate) const DAMAGE_LIMIT: u32 = 300;

/// The value of a C enum field. libavformat copies colour tags from the
/// file unchecked (Matroska's `MatrixCoefficients`, for one), and a value
/// the bindings do not name must never be read as their Rust enum.
///
/// # Safety
/// `field` points at a live, initialised C enum.
pub(crate) unsafe fn raw_enum<T>(field: *const T) -> u32 {
    const { assert!(std::mem::size_of::<T>() == 4) };
    field.cast::<u32>().read()
}

/// What a decoder returns for a packet it cannot use: damaged data, or a
/// feature the damage appears to ask for (AAC gain control is one flag bit).
/// The FFmpeg CLI skips both and decodes on.
pub(crate) fn is_damage(error: &ffmpeg::Error) -> bool {
    matches!(
        error,
        ffmpeg::Error::InvalidData | ffmpeg::Error::PatchWelcome
    )
}

/// Without a container index, decode forward rather than seek only this far.
pub(crate) const FORWARD_WITHOUT_INDEX: f64 = 0.5;
/// Still decoders keep this many pictures before the current one, so a
/// step back does not seek and decode a GOP again.
const RECENT_PICTURES: usize = 8;
/// ...or fewer when they would hold more decoded bytes than this (4K keeps 5).
const RECENT_BYTES: usize = 64 << 20;

static INIT: Once = Once::new();

pub(crate) fn ensure_init() {
    INIT.call_once(|| {
        let _ = ffmpeg::init();
        // Seek and skip-sample notices are routine for preview and audio.
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
    });
}

fn err(context: &str, error: ffmpeg::Error) -> String {
    format!("{context}: {error}")
}

pub struct VideoDecoder {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    scaler: Scaler,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    source_origin: i64,
    target_w: u32,
    target_h: u32,
    fast: bool,
    /// Clockwise quarter turns from the stream's display matrix.
    turns: u8,
    decoded: ffmpeg::frame::Video,
    /// Presentation time of `decoded` once it was returned.
    decoded_pts: Option<f64>,
    lookahead: ffmpeg::frame::Video,
    lookahead_pts: Option<f64>,
    seek_target: Option<f64>,
    eof_sent: bool,
    /// The decoder returned its final picture since the last seek.
    drained: bool,
    /// Consecutive pictures just before `decoded` (references, not copies).
    recent: VecDeque<(f64, ffmpeg::frame::Video)>,
    recent_limit: usize,
    /// Opened with a hardware device (or the test fault), and pictures decoded.
    pub(crate) hardware: bool,
    hardware_pictures: u32,
    /// Damaged packets and pictures skipped since the last good picture.
    damage: u32,
    /// The last downloaded GPU surface.
    transfer: ffmpeg::frame::Video,
}

// SAFETY: every field is exclusively owned. ffmpeg-next marks the demuxer,
// codec context and frames `Send` but not the scaler, which holds a raw
// SwsContext pointer; libswscale contexts have no thread affinity and are
// only used through `&mut self`. Preview lanes move a parked decoder between
// threads; it is never shared.
unsafe impl Send for VideoDecoder {}

impl VideoDecoder {
    pub fn open(path: &str, target_w: u32, fast: bool) -> MediaResult<Self> {
        Self::open_threaded(
            path,
            target_w,
            fast,
            ffmpeg::codec::threading::Type::Frame,
            false,
        )
    }

    /// A preview playback decoder, on the GPU when hardware decode is on and
    /// the file is expensive in software. Stills and scrubbing stay in
    /// software: a hardware far seek decodes its GOP on one thread, which
    /// halved the pictures shown while scrubbing (6/s vs 12/s on 4K HEVC).
    pub(crate) fn open_playback(path: &str, target_w: u32) -> MediaResult<Self> {
        Self::open_threaded(
            path,
            target_w,
            true,
            ffmpeg::codec::threading::Type::Frame,
            true,
        )
    }

    /// Frame threading pipelines consecutive pictures; slice threading
    /// splits one picture, which is what isolated keyframes need.
    fn open_threaded(
        path: &str,
        target_w: u32,
        fast: bool,
        threading: ffmpeg::codec::threading::Type,
        hardware: bool,
    ) -> MediaResult<Self> {
        ensure_init();
        let input = ffmpeg::format::input(path).map_err(|e| err("open input", e))?;
        let (stream_index, time_base, stream_start, parameters, turns) = {
            let stream = input
                .streams()
                .best(ffmpeg::media::Type::Video)
                .ok_or_else(|| "no video stream".to_owned())?;
            let turns = display_turns(&stream);
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

        let open_codec = |hardware: bool| -> MediaResult<(ffmpeg::decoder::Video, bool)> {
            let mut context = ffmpeg::codec::context::Context::from_parameters(parameters.clone())
                .map_err(|e| err("decoder parameters", e))?;
            context.set_threading(ffmpeg::codec::threading::Config {
                kind: threading,
                count: 0, // libavcodec chooses based on CPU cores.
            });
            let attached = hardware
                && (attach(&mut context)
                    || (INJECT_FAULT.load(Ordering::Relaxed) && !hardware_broken()));
            if attached {
                // The GPU decodes; threads only parse ahead. Each frame thread
                // can finish a picture before any is synced, which overflows
                // the driver's ring (see `sync_surface`); 4K HEVC also ran no
                // faster with 2 threads and slower with 4.
                context.set_threading(ffmpeg::codec::threading::Config {
                    kind: threading,
                    count: 1,
                });
            }
            let decoder = context
                .decoder()
                .video()
                .map_err(|e| err("open video decoder", e))?;
            Ok((decoder, attached))
        };
        // A driver that cannot open the codec falls back to software here,
        // without reopening the input.
        let (decoder, attached) = match open_codec(hardware) {
            Err(error)
                if hardware && !hardware_broken() && HARDWARE_DECODE.load(Ordering::Relaxed) =>
            {
                let software = open_codec(false)?;
                hardware_failed(&error);
                software
            }
            result => result?,
        };

        let (scaler, target_h) = Scaler::new(
            (decoder.format(), decoder.width(), decoder.height()),
            turns,
            target_w,
            fast,
        )?;
        Ok(Self {
            input,
            decoder,
            scaler,
            stream_index,
            time_base,
            source_origin,
            target_w,
            target_h,
            fast,
            turns,
            decoded: ffmpeg::frame::Video::empty(),
            decoded_pts: None,
            lookahead: ffmpeg::frame::Video::empty(),
            lookahead_pts: None,
            seek_target: None,
            eof_sent: false,
            drained: false,
            recent: VecDeque::new(),
            recent_limit: 0,
            hardware: attached,
            hardware_pictures: 0,
            damage: 0,
            transfer: ffmpeg::frame::Video::empty(),
        })
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.target_w, self.target_h)
    }

    /// Change the output width or scaling quality without moving the
    /// decoder, so scrub and settle sizes share one position.
    pub(crate) fn set_output(&mut self, target_w: u32, fast: bool) -> MediaResult<()> {
        if (target_w, fast) != (self.target_w, self.fast) {
            let (scaler, target_h) = Scaler::new(self.scaler.source, self.turns, target_w, fast)?;
            self.scaler = scaler;
            (self.target_w, self.target_h, self.fast) = (target_w, target_h, fast);
        }
        Ok(())
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
        self.drained = false;
        self.damage = 0;
        self.decoded_pts = None;
        self.lookahead_pts = None;
        self.recent.clear();
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
                ffmpeg::ffi::AVSEEK_FLAG_BACKWARD,
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

    /// Skip one piece of damaged data, or fail with `error` once a run of
    /// them is longer than a damaged stretch plausibly is. A hardware
    /// decoder fails at once, so the session falls back to software first.
    fn damaged(&mut self, error: String) -> MediaResult<()> {
        self.damage += 1;
        if self.hardware || self.damage > DAMAGE_LIMIT {
            return Err(error);
        }
        Ok(())
    }

    /// Decode one frame, including delayed frames after send_eof. Only a
    /// receive-side EOF means the decoder has actually finished draining.
    fn receive_decoded(&mut self) -> MediaResult<Option<f64>> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    self.damage = 0;
                    if self.hardware {
                        self.hardware_pictures += 1;
                        if INJECT_FAULT.load(Ordering::Relaxed) && self.hardware_pictures > 5 {
                            return Err("injected hardware decode failure".to_owned());
                        }
                    }
                    sync_surface(&self.decoded)?;
                    return Ok(Some(self.frame_seconds(&self.decoded)));
                }
                Err(ffmpeg::Error::Eof) => {
                    self.drained = true;
                    return Ok(None);
                }
                Err(ffmpeg::Error::Other { errno })
                    if errno == ffmpeg::error::EAGAIN && !self.eof_sent => {}
                // Damaged data costs its pictures, not the file: the decoder
                // picks up at a later packet, as the FFmpeg CLI does.
                Err(e) if is_damage(&e) && !self.eof_sent => {
                    self.damaged(err("receive frame", e))?;
                    continue;
                }
                Err(e) => return Err(err("receive frame", e)),
            }
            // A receive-side EAGAIN guarantees that sending can make progress.
            // Read directly so a demux error cannot masquerade as clean EOF.
            loop {
                let mut packet = ffmpeg::Packet::empty();
                match packet.read(&mut self.input) {
                    Ok(()) if packet.stream() == self.stream_index => {
                        match self.decoder.send_packet(&packet) {
                            Ok(()) => {}
                            Err(e) if is_damage(&e) => {
                                self.damaged(err("send packet", e))?;
                            }
                            Err(e) => return Err(err("send packet", e)),
                        }
                        break;
                    }
                    Ok(()) => continue,
                    Err(ffmpeg::Error::InvalidData) => {
                        self.damaged(err("read packet", ffmpeg::Error::InvalidData))?;
                    }
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
        if let Some(pts) = self.decoded_pts.take() {
            self.remember(pts, false);
        }
        let seconds = if let Some(pts) = self.lookahead_pts.take() {
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            Some(pts)
        } else {
            self.receive_decoded()?
        };
        let Some(target) = self.seek_target.take() else {
            self.decoded_pts = seconds;
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
                self.remember(seconds, true);
                seconds = pts;
                continue;
            }
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            self.lookahead_pts = next;
            self.decoded_pts = Some(seconds);
            // [seconds, next): equality selected the new frame above. At
            // actual EOF (or before the first PTS), clamp to the nearest frame.
            return Ok(Some(seconds));
        }
    }

    /// Keep a reference to a picture being passed over (`decoded`, or the
    /// candidate parked in `lookahead`) when this decoder keeps recent ones.
    fn remember(&mut self, pts: f64, parked: bool) {
        if self.recent_limit == 0 {
            return;
        }
        let picture = if parked {
            &self.lookahead
        } else {
            &self.decoded
        };
        // SAFETY: av_frame_clone adds a reference to the same buffers; the
        // result is owned by the wrapper, or null on allocation failure.
        let reference = unsafe { ffmpeg::ffi::av_frame_clone(picture.as_ptr()) };
        if reference.is_null() {
            // Recent pictures must stay consecutive; start over instead.
            self.recent.clear();
            return;
        }
        let reference = unsafe { ffmpeg::frame::Video::wrap(reference) };
        let bytes: usize = (0..reference.planes())
            .map(|plane| reference.data(plane).len())
            .sum();
        let keep = (RECENT_BYTES / bytes.max(1)).clamp(1, self.recent_limit);
        self.recent.push_back((pts, reference));
        while self.recent.len() > keep {
            self.recent.pop_front();
        }
    }

    /// The picture covering `time`, reusing the decoder's position: the
    /// current or a recent picture is rescaled, a later time that is near or
    /// in the same GOP decodes forward, and anything else seeks. Matches
    /// `frame_at`.
    pub(crate) fn still_at(&mut self, time: f64) -> MediaResult<RawFrame> {
        let time = time.max(0.0);
        if let Some(current) = self.decoded_pts {
            if time < current {
                // Recent pictures are consecutive: each lasts until the next.
                if let Some(index) = self.recent.iter().rposition(|(pts, _)| *pts <= time) {
                    return self.scale_into(Some(index), Vec::new());
                }
            } else if self.lookahead_pts.map_or(self.drained, |next| time < next) {
                return self.scale_decoded_into(Vec::new());
            } else {
                // A seek restarts frame threading, which costs more than
                // decoding a short way on even across a keyframe.
                let forward = time - current <= FORWARD_WITHOUT_INDEX
                    || self
                        .keyframe_before(time)
                        .is_some_and(|keyframe| keyframe <= current + 1e-6);
                if forward {
                    self.skip_to(time);
                    self.next_decoded()?
                        .ok_or_else(|| "no frame decoded at requested time".to_owned())?;
                    return self.scale_decoded_into(Vec::new());
                }
            }
        }
        self.frame_at(time)
    }

    /// Presentation time of the picture after the current one, decoding it
    /// without scaling (or downloading it from the GPU). None at EOF.
    pub(crate) fn peek_next(&mut self) -> MediaResult<Option<f64>> {
        if self.lookahead_pts.is_none() && !self.drained {
            // Decode into the lookahead slot; the current picture stays put.
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            let next = self.receive_decoded();
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            self.lookahead_pts = next?;
        }
        Ok(self.lookahead_pts)
    }

    /// Make the latest picture at or before `target` current, passing over
    /// earlier ones unscaled. Stays on the current picture when the next one
    /// starts after `target`. Returns the current picture's time.
    pub(crate) fn advance_to(&mut self, target: f64) -> MediaResult<Option<f64>> {
        while let Some(next) = self.peek_next()?.filter(|next| *next <= target) {
            if let Some(pts) = self.decoded_pts.take() {
                self.remember(pts, false);
            }
            std::mem::swap(&mut self.decoded, &mut self.lookahead);
            self.lookahead_pts = None;
            self.decoded_pts = Some(next);
        }
        Ok(self.decoded_pts)
    }

    /// Scale the current picture.
    pub(crate) fn scale_current(&mut self, buffer: Vec<u8>) -> MediaResult<RawFrame> {
        self.scale_into(None, buffer)
    }

    fn scale_decoded_into(&mut self, buffer: Vec<u8>) -> MediaResult<RawFrame> {
        self.scale_into(None, buffer)
    }

    /// Scale `decoded`, or the recent picture at `recent`, into `buffer`.
    fn scale_into(&mut self, recent: Option<usize>, mut buffer: Vec<u8>) -> MediaResult<RawFrame> {
        let mut picture = match recent {
            Some(index) => &self.recent[index].1,
            None => &self.decoded,
        };
        // Surfaces stay on the GPU until shown, so seek preroll copies nothing.
        if on_gpu(picture) {
            download(picture, &mut self.transfer)?;
            picture = &self.transfer;
        }
        let source = (picture.format(), picture.width(), picture.height());
        if source != self.scaler.source {
            // A download changes only the format. A mid-stream size change
            // stays unsupported: the output size was fixed at open.
            let (_, width, height) = self.scaler.source;
            if (source.1, source.2) != (width, height) {
                return Err("video size changed mid-stream".to_owned());
            }
            self.scaler = Scaler::new(source, self.turns, self.target_w, self.fast)?.0;
        }
        let scaled = self.scaler.run(picture)?;
        let stride = scaled.stride(0);
        let width = self.target_w as usize;
        let height = self.target_h as usize;
        buffer.resize(width * height * 4, 0);
        let data = scaled.data(0);
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

/// Clockwise quarter turns that display `stream`, from its display matrix.
pub(crate) fn display_turns(stream: &ffmpeg::format::stream::Stream) -> u8 {
    stream
        .side_data()
        .find(|data| {
            data.kind() == ffmpeg::codec::packet::side_data::Type::DisplayMatrix
                && data.data().len() >= 36
        })
        .map_or(0, |data| {
            // SAFETY: a display matrix is nine i32 values in an
            // av_malloc-aligned side-data buffer owned by the stream.
            let degrees =
                unsafe { ffmpeg::ffi::av_display_rotation_get(data.data().as_ptr().cast()) };
            clockwise_turns(degrees)
        })
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

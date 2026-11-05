//! In-process video decoding via ffmpeg-next (libavcodec).
//!
//! Replaces process-per-frame scrubbing and pipe streaming: one decoder
//! seeks and yields scaled RGBA frames, with libavcodec frame threading.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Once;
use std::thread;

use ffmpeg::Rescale;
use ffmpeg_next as ffmpeg;

use crate::{scaled_height, MediaResult, RawFrame};

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

/// EXPERIMENT: built with the `vaapi` feature, `EDDIE_HWDEC=vaapi` decodes
/// on a VAAPI device. libavcodec picks the hardware format when the profile
/// is supported and silently falls back to software otherwise.
fn attach_vaapi(context: &mut ffmpeg::codec::context::Context) -> bool {
    if !cfg!(feature = "vaapi") || std::env::var("EDDIE_HWDEC").as_deref() != Ok("vaapi") {
        return false;
    }
    // One device serves every decoder: creating one costs about 250 ms.
    struct Device(*mut ffmpeg::ffi::AVBufferRef);
    // SAFETY: the reference is created once, never freed, and only passed to
    // av_buffer_ref, which is thread-safe.
    unsafe impl Send for Device {}
    unsafe impl Sync for Device {}
    static DEVICE: std::sync::OnceLock<Device> = std::sync::OnceLock::new();
    let device = DEVICE.get_or_init(|| {
        let mut device = std::ptr::null_mut();
        // SAFETY: on failure the pointer stays null.
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device,
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                c"/dev/dri/renderD128".as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        Device(if result < 0 {
            std::ptr::null_mut()
        } else {
            device
        })
    });
    if device.0.is_null() {
        return false;
    }
    // SAFETY: the codec context is not open yet; it takes its own reference.
    unsafe {
        let raw = context.as_mut_ptr();
        (*raw).hw_device_ctx = ffmpeg::ffi::av_buffer_ref(device.0);
        // Recent pictures, the lookahead and the current one hold surfaces.
        (*raw).extra_hw_frames = RECENT_PICTURES as i32 + 4;
    }
    true
}

// Only the `vaapi` feature links libva, so default builds need no VA headers.
#[cfg(feature = "vaapi")]
#[link(name = "va")]
extern "C" {
    fn vaSyncSurface(display: *mut std::ffi::c_void, surface: u32) -> i32;
}

/// Without the feature no decoder has a VAAPI device, so no picture gets here.
#[cfg(not(feature = "vaapi"))]
#[allow(non_snake_case)]
unsafe fn vaSyncSurface(_display: *mut std::ffi::c_void, _surface: u32) -> i32 {
    0
}

/// Wait until a decoded VAAPI surface is complete, as it leaves the decoder.
/// nvidia-vaapi-driver (0.0.18) queues every finished picture on a 16-slot
/// ring with no overflow check; pictures decoded ahead and never synced (seek
/// preroll, frame threads) lap it, and a lost picture never completes, so a
/// later sync on it waits forever. Syncing here keeps the ring drained.
fn sync_surface(picture: &ffmpeg::frame::Video) -> MediaResult<()> {
    if picture.format() != ffmpeg::format::Pixel::VAAPI {
        return Ok(());
    }
    // SAFETY: a VAAPI frame carries its surface ID in data[3] and a frames
    // context whose device context is an AVVAAPIDeviceContext.
    let status = unsafe {
        let frame = picture.as_ptr();
        let frames = (*(*frame).hw_frames_ctx).data as *const ffmpeg::ffi::AVHWFramesContext;
        let device = (*(*frames).device_ctx).hwctx as *const ffmpeg::ffi::AVVAAPIDeviceContext;
        vaSyncSurface(
            (*device).display as *mut _,
            (*frame).data[3] as usize as u32,
        )
    };
    if status != 0 {
        return Err(format!("sync surface: VA status {status}"));
    }
    Ok(())
}

/// Copy a VAAPI surface into `into` (NV12 or P010) with its timing and
/// colour. `into` keeps its buffers between pictures of the same size: a
/// fresh 4K allocation per picture cost page faults on every frame.
fn download(picture: &ffmpeg::frame::Video, into: &mut ffmpeg::frame::Video) -> MediaResult<()> {
    // SAFETY: both frames are owned and `into` is never shared, so its
    // buffers are writable; transfer allocates them when it has none.
    unsafe {
        let (src, dst) = (picture.as_ptr(), into.as_mut_ptr());
        if (*dst).buf[0].is_null() || ((*dst).width, (*dst).height) != ((*src).width, (*src).height)
        {
            ffmpeg::ffi::av_frame_unref(dst);
        }
        let result = ffmpeg::ffi::av_hwframe_transfer_data(dst, src, 0);
        if result < 0 {
            return Err(err("download surface", ffmpeg::Error::from(result)));
        }
        (*dst).pts = (*src).pts;
        (*dst).color_range = (*src).color_range;
        (*dst).colorspace = (*src).colorspace;
        (*dst).color_primaries = (*src).color_primaries;
        (*dst).color_trc = (*src).color_trc;
        (*dst).chroma_location = (*src).chroma_location;
    }
    Ok(())
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
    /// EXPERIMENT: a downloaded VAAPI surface.
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
        Self::open_threaded(path, target_w, fast, ffmpeg::codec::threading::Type::Frame)
    }

    /// Frame threading pipelines consecutive pictures; slice threading
    /// splits one picture, which is what isolated keyframes need.
    fn open_threaded(
        path: &str,
        target_w: u32,
        fast: bool,
        threading: ffmpeg::codec::threading::Type,
    ) -> MediaResult<Self> {
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
            kind: threading,
            count: 0, // libavcodec chooses based on CPU cores.
        });
        if attach_vaapi(&mut context) {
            // The GPU decodes; threads only parse ahead. Each frame thread can
            // finish a picture before any is synced, which overflows the
            // driver's ring (see `sync_surface`); 4K HEVC also ran no faster
            // with 2 threads and slower with 4.
            context.set_threading(ffmpeg::codec::threading::Config {
                kind: threading,
                count: 1,
            });
        }
        let decoder = context
            .decoder()
            .video()
            .map_err(|e| err("open video decoder", e))?;

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
                Ok(()) => {
                    sync_surface(&self.decoded)?;
                    return Ok(Some(self.frame_seconds(&self.decoded)));
                }
                Err(ffmpeg::Error::Eof) => {
                    self.drained = true;
                    return Ok(None);
                }
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
        if picture.format() == ffmpeg::format::Pixel::VAAPI {
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

/// Source pictures to display-size RGBA in two swscale passes: resize in
/// the source format, then convert at the small size with each picture's
/// own matrix and range, as export does. One combined pass cost about
/// 2.8 ms per 4K picture against about 1.25 ms for both, and read every
/// source as BT.601, which oversaturated BT.709 HD footage.
struct Scaler {
    resize: ffmpeg::software::scaling::Context,
    convert: ffmpeg::software::scaling::Context,
    resized: ffmpeg::frame::Video,
    rgba: ffmpeg::frame::Video,
    /// swscale matrix and full-range flag last applied to `convert`.
    colors: Option<(i32, i32)>,
    /// Input format and size this scaler reads.
    source: (ffmpeg::format::Pixel, u32, u32),
}

impl Scaler {
    /// A scaler for `target_w` display pixels, and the display height.
    fn new(
        (source, width, height): (ffmpeg::format::Pixel, u32, u32),
        turns: u8,
        target_w: u32,
        fast: bool,
    ) -> MediaResult<(Self, u32)> {
        use ffmpeg::format::Pixel;
        use ffmpeg::software::scaling::{Context, Flags};
        // target_w/target_h are display dimensions. A quarter-turn scales to
        // the transposed size, then rotates while copying out.
        let quarter = turns % 2 == 1;
        let target_h = if quarter {
            scaled_height(height, width, target_w)
        } else {
            scaled_height(width, height, target_w)
        };
        let (scaled_w, scaled_h) = if quarter {
            (target_h, target_w)
        } else {
            (target_w, target_h)
        };
        let flags = if fast {
            Flags::FAST_BILINEAR
        } else {
            Flags::BICUBIC
        };
        // SAFETY: a pure query on a pixel format value.
        let resizable = unsafe { ffmpeg::ffi::sws_isSupportedOutput(source.into()) } > 0;
        let middle = if resizable { source } else { Pixel::YUV420P };
        let resize = Context::get(source, width, height, middle, scaled_w, scaled_h, flags)
            .map_err(|e| err("create scaler", e))?;
        let convert = Context::get(
            middle,
            scaled_w,
            scaled_h,
            Pixel::RGBA,
            scaled_w,
            scaled_h,
            flags,
        )
        .map_err(|e| err("create converter", e))?;
        let scaler = Self {
            resize,
            convert,
            resized: ffmpeg::frame::Video::empty(),
            rgba: ffmpeg::frame::Video::empty(),
            colors: None,
            source: (source, width, height),
        };
        Ok((scaler, target_h))
    }

    fn run(&mut self, picture: &ffmpeg::frame::Video) -> MediaResult<&ffmpeg::frame::Video> {
        use ffmpeg::color::{Range, Space};
        use ffmpeg::ffi;
        // Untagged matrices use swscale's default (BT.601), as the FFmpeg
        // CLI and export do; yuvj formats are full range by definition.
        let matrix = match picture.color_space() {
            Space::BT709 => ffi::SWS_CS_ITU709,
            Space::FCC => ffi::SWS_CS_FCC,
            Space::SMPTE240M => ffi::SWS_CS_SMPTE240M,
            Space::BT2020NCL | Space::BT2020CL => ffi::SWS_CS_BT2020,
            _ => ffi::SWS_CS_DEFAULT,
        };
        let full = match picture.color_range() {
            Range::JPEG => 1,
            Range::MPEG => 0,
            _ => i32::from(matches!(
                picture.format(),
                ffmpeg::format::Pixel::YUVJ420P
                    | ffmpeg::format::Pixel::YUVJ422P
                    | ffmpeg::format::Pixel::YUVJ444P
                    | ffmpeg::format::Pixel::YUVJ440P
            )),
        };
        if self.colors != Some((matrix, full)) {
            // SAFETY: the context is owned and initialized; coefficient
            // tables are static. RGB output is always full range.
            unsafe {
                ffi::sws_setColorspaceDetails(
                    self.convert.as_mut_ptr(),
                    ffi::sws_getCoefficients(matrix),
                    full,
                    ffi::sws_getCoefficients(ffi::SWS_CS_DEFAULT),
                    1,
                    0,
                    1 << 16,
                    1 << 16,
                );
            }
            self.colors = Some((matrix, full));
        }
        self.resize
            .run(picture, &mut self.resized)
            .map_err(|e| err("scale frame", e))?;
        self.convert
            .run(&self.resized, &mut self.rgba)
            .map_err(|e| err("convert frame", e))?;
        Ok(&self.rgba)
    }
}

/// Filmstrip stills at `times` (ascending), keyed by each still's PTS.
/// A sample within `tolerance` after its keyframe shows that keyframe,
/// decoding keyframes only; a sample deep inside a long GOP (or without an
/// index) decodes forward to its exact picture instead.
pub(crate) fn filmstrip_stills(
    path: &str,
    times: &[f64],
    target_w: u32,
    tolerance: f64,
) -> MediaResult<Vec<(f64, RawFrame)>> {
    let mut decoder =
        VideoDecoder::open_threaded(path, target_w, true, ffmpeg::codec::threading::Type::Slice)?;
    let mut stills: Vec<(f64, RawFrame)> = Vec::new();
    let mut position: Option<f64> = None;
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
        } else {
            decoder.decoder.skip_frame(ffmpeg::Discard::Default);
            if same_gop || (key.is_none() && position.is_some()) {
                decoder.skip_to(time);
            } else {
                decoder.seek(time)?;
            }
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

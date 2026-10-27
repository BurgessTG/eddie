//! In-process video decoding via ffmpeg-next (libavcodec).
//!
//! Replaces process-per-frame scrubbing and pipe streaming: one decoder
//! seeks and yields scaled RGBA frames, with libavcodec frame threading.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Once;
use std::thread;

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
    time_base: f64,
    frame_duration: f64,
    target_w: u32,
    target_h: u32,
    decoded: ffmpeg::frame::Video,
    scaled: ffmpeg::frame::Video,
    eof_sent: bool,
}

impl VideoDecoder {
    pub fn open(path: &str, target_w: u32, fast: bool) -> MediaResult<Self> {
        ensure_init();
        let input = ffmpeg::format::input(path).map_err(|e| err("open input", e))?;
        let (stream_index, time_base, frame_rate, parameters) = {
            let stream = input
                .streams()
                .best(ffmpeg::media::Type::Video)
                .ok_or_else(|| "no video stream".to_owned())?;
            (
                stream.index(),
                f64::from(stream.time_base()),
                f64::from(stream.avg_frame_rate()),
                stream.parameters(),
            )
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

        let target_h = scaled_height(decoder.width(), decoder.height(), target_w);
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
            target_w,
            target_h,
            flags,
        )
        .map_err(|e| err("create scaler", e))?;

        Ok(Self {
            input,
            decoder,
            scaler,
            stream_index,
            time_base,
            frame_duration: if frame_rate > 0.0 {
                1.0 / frame_rate
            } else {
                1.0 / 30.0
            },
            target_w,
            target_h,
            decoded: ffmpeg::frame::Video::empty(),
            scaled: ffmpeg::frame::Video::empty(),
            eof_sent: false,
        })
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.target_w, self.target_h)
    }

    pub fn seek(&mut self, time: f64) -> MediaResult<()> {
        let ts = (time.max(0.0) * ffmpeg::ffi::AV_TIME_BASE as f64) as i64;
        self.input.seek(ts, ..).map_err(|e| err("seek", e))?;
        self.decoder.flush();
        self.eof_sent = false;
        Ok(())
    }

    fn frame_seconds(&self, frame: &ffmpeg::frame::Video) -> f64 {
        frame
            .timestamp()
            .map(|ts| ts as f64 * self.time_base)
            .unwrap_or(0.0)
    }

    /// Decode the next frame. Returns its presentation time in seconds.
    fn receive_decoded(&mut self) -> Option<f64> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => return Some(self.frame_seconds(&self.decoded)),
                Err(_) => {
                    if self.eof_sent {
                        return None;
                    }
                    // Feed one packet, then retry receiving.
                    loop {
                        match self.input.packets().next() {
                            Some((stream, packet)) if stream.index() == self.stream_index => {
                                if self.decoder.send_packet(&packet).is_err() {
                                    // Decoder was full; outer loop drains it.
                                }
                                break;
                            }
                            Some(_) => continue,
                            None => {
                                let _ = self.decoder.send_eof();
                                self.eof_sent = true;
                                break;
                            }
                        }
                    }
                }
            }
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
        for row in 0..height {
            let src = row * stride;
            let dst = row * width * 4;
            buffer[dst..dst + width * 4].copy_from_slice(&data[src..src + width * 4]);
        }
        Ok(RawFrame {
            width: self.target_w,
            height: self.target_h,
            rgba: buffer,
        })
    }

    /// Next streamed frame plus its presentation time.
    pub fn next_frame(&mut self, buffer: Vec<u8>) -> Option<(f64, RawFrame)> {
        let seconds = self.receive_decoded()?;
        let frame = self.scale_decoded_into(buffer).ok()?;
        Some((seconds, frame))
    }

    /// Seek and return the frame covering `time`.
    pub fn frame_at(&mut self, time: f64) -> MediaResult<RawFrame> {
        self.seek(time)?;
        loop {
            let seconds = self
                .receive_decoded()
                .ok_or_else(|| "no frame decoded at requested time".to_owned())?;
            if seconds + self.frame_duration >= time || self.eof_sent {
                return self.scale_decoded_into(Vec::new());
            }
        }
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

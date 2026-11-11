//! One clip's sound: decode, resample to the mix format, clean, fade.

use std::sync::mpsc::{Receiver, SyncSender};

use ffmpeg::Rescale;
use ffmpeg_next as ffmpeg;

use super::mixer::Chunk;
use super::{sample, RATE};
use crate::{AudioSegment, MediaResult, AUDIO_RATE};

/// Decode `segment` from timeline sample `first` to `end` into chunks.
pub(super) fn decode_voice(
    segment: &AudioSegment,
    first: u64,
    end: u64,
    chunks: &SyncSender<MediaResult<Chunk>>,
    recycle: &Receiver<Vec<f32>>,
) -> MediaResult<()> {
    let mut source = Source::open(&segment.path)?;
    let start = sample(segment.start);
    let source_start = sample(segment.source_in) as i64 - start as i64;
    // Source sample wanted at timeline sample `first`.
    let wanted = first as i64 + source_start;
    source.seek(wanted as f64 / RATE)?;

    let mut resampler = Resampler::default();
    let mut frame = ffmpeg::frame::Audio::empty();
    // Source sample (at RATE) of the next resampled frame.
    let mut cursor: Option<i64> = None;
    let mut cleanup = segment
        .clean_voice
        .then(|| crate::voice::VoiceCleanup::new(RATE));
    while source.next_frame(&mut frame)? {
        if frame.rate() == 0 || frame.samples() == 0 {
            continue;
        }
        let shape = shape_of(&frame);
        if !resampler.fits(shape) {
            if let Some(cleanup) = cleanup.as_mut() {
                // Upmixing puts mono 3 dB down on each channel; export
                // compresses the mono source itself, so detect at its level.
                cleanup.set_mono(shape.1.channels() == 1);
            }
            // Damage can decode without an error into a shape nothing
            // plays (38 channels from a garbage AAC packet).
            if let Err(e) = resampler.rebuild(shape) {
                source.damage.hit("audio resampler", e)?;
                continue;
            }
        }
        source.damage.clear();
        if let Some(pts) = frame.timestamp() {
            let at = source.sample_at(pts);
            // Trust timestamps only across real discontinuities; resampled
            // frame counts drift by a sample against rounded timestamps.
            if cursor.is_none_or(|c| (c - at).abs() > (RATE * 0.02) as i64) {
                cursor = Some(at);
            }
        }
        let samples = resampler.convert(&frame)?;
        let produced = samples.len() / 2;
        let block_source = cursor.unwrap_or(0);
        cursor = Some(block_source + produced as i64);
        // Timeline position of this block, then keep [first, end).
        let block_at = block_source - source_start;
        let keep_from = (first as i64 - block_at).clamp(0, produced as i64) as usize;
        let keep_to = (end as i64 - block_at).clamp(0, produced as i64) as usize;
        if keep_to > keep_from {
            let mut data = recycle.try_recv().unwrap_or_default();
            data.clear();
            data.extend_from_slice(&samples[keep_from * 2..keep_to * 2]);
            if let Some(cleanup) = cleanup.as_mut() {
                cleanup.process(&mut data);
            }
            let chunk = Chunk {
                at: (block_at + keep_from as i64) as u64,
                data,
            };
            if chunks.send(Ok(chunk)).is_err() {
                return Ok(());
            }
        }
        if block_at + produced as i64 >= end as i64 {
            return Ok(());
        }
    }
    Ok(())
}

fn fail(context: &str, error: ffmpeg::Error) -> String {
    format!("{context}: {error}")
}

/// Damaged packets in a row. Each costs its own sound, as in the FFmpeg
/// CLI; a stream that never recovers is reported, not played as silence.
#[derive(Default)]
struct Damage(u32);

impl Damage {
    fn hit(&mut self, context: &str, error: ffmpeg::Error) -> MediaResult<()> {
        self.0 += 1;
        if self.0 > crate::decode::DAMAGE_LIMIT {
            return Err(fail(context, error));
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

/// A file's best audio stream and its decoder.
struct Source {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::codec::decoder::Audio,
    index: usize,
    time_base: ffmpeg::Rational,
    /// The container's start, in the stream's time base: source time zero.
    origin: i64,
    eof: bool,
    damage: Damage,
}

impl Source {
    fn open(path: &str) -> MediaResult<Self> {
        crate::decode::ensure_init();
        let input = ffmpeg::format::input(path).map_err(|e| fail("open audio", e))?;
        let (index, time_base, stream_start, parameters) = {
            let stream = input
                .streams()
                .best(ffmpeg::media::Type::Audio)
                .ok_or_else(|| "no audio stream".to_owned())?;
            (
                stream.index(),
                stream.time_base(),
                stream.start_time(),
                stream.parameters(),
            )
        };
        // Same container-relative source clock as the video decoder.
        let format_start = unsafe { (*input.as_ptr()).start_time };
        let origin = if format_start != ffmpeg::ffi::AV_NOPTS_VALUE {
            format_start.rescale(ffmpeg::rescale::TIME_BASE, time_base)
        } else if stream_start != ffmpeg::ffi::AV_NOPTS_VALUE {
            stream_start
        } else {
            0
        };
        let decoder = ffmpeg::codec::context::Context::from_parameters(parameters)
            .map_err(|e| fail("audio parameters", e))?
            .decoder()
            .audio()
            .map_err(|e| fail("open audio decoder", e))?;
        Ok(Self {
            input,
            decoder,
            index,
            time_base,
            origin,
            eof: false,
            damage: Damage::default(),
        })
    }

    /// Read from 0.1 s before `seconds` of source time. A time of zero or
    /// earlier reads from the top without a seek.
    fn seek(&mut self, seconds: f64) -> MediaResult<()> {
        if seconds <= 0.0 {
            return Ok(());
        }
        let ts = self
            .origin
            .rescale(self.time_base, ffmpeg::rescale::TIME_BASE)
            .saturating_add(((seconds - 0.1).max(0.0) * ffmpeg::ffi::AV_TIME_BASE as f64) as i64);
        self.input.seek(ts, ..ts).map_err(|e| fail("audio seek", e))
    }

    /// The source sample (at `RATE`) a frame stamped `pts` starts on.
    fn sample_at(&self, pts: i64) -> i64 {
        let seconds = pts.saturating_sub(self.origin) as f64
            * f64::from(self.time_base.numerator())
            / f64::from(self.time_base.denominator());
        (seconds * RATE).round() as i64
    }

    /// Decode the next frame into `frame`; false once the stream has ended.
    /// Only the demuxer's own end of file ends the voice: any other read or
    /// decode error is reported, since the rest of the clip would otherwise
    /// play and measure as silence.
    fn next_frame(&mut self, frame: &mut ffmpeg::frame::Audio) -> MediaResult<bool> {
        loop {
            match self.decoder.receive_frame(frame) {
                Ok(()) => return Ok(true),
                // Nothing is left to read, so a failed drain loses at most
                // the decoder's delay.
                Err(_) if self.eof => return Ok(false),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                    self.feed()?;
                }
                Err(e) if crate::decode::is_damage(&e) => self.damage.hit("decode audio", e)?,
                Err(e) => return Err(fail("decode audio", e)),
            }
        }
    }

    /// Send the decoder the file's next packet, or the end of the stream.
    fn feed(&mut self) -> MediaResult<()> {
        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut self.input) {
            Ok(()) if packet.stream() == self.index => match self.decoder.send_packet(&packet) {
                Ok(()) => Ok(()),
                Err(e) if crate::decode::is_damage(&e) => self.damage.hit("send audio packet", e),
                Err(e) => Err(fail("send audio packet", e)),
            },
            Ok(()) => Ok(()),
            Err(e @ ffmpeg::Error::InvalidData) => self.damage.hit("read audio packet", e),
            Err(ffmpeg::Error::Eof) => {
                self.decoder
                    .send_eof()
                    .map_err(|e| fail("end audio stream", e))?;
                self.eof = true;
                Ok(())
            }
            Err(e) => Err(fail("read audio packet", e)),
        }
    }
}

/// What a resampler is built for: a frame's format, channels and rate.
type Shape = (ffmpeg::format::Sample, ffmpeg::ChannelLayout, u32);

fn shape_of(frame: &ffmpeg::frame::Audio) -> Shape {
    let layout = if frame.channel_layout().channels() > 0 {
        frame.channel_layout()
    } else {
        ffmpeg::ChannelLayout::default(i32::from(frame.channels()))
    };
    (frame.format(), layout, frame.rate())
}

/// Converts decoded frames to interleaved stereo at the mix rate. A stream
/// may change format, channels or rate part-way (broadcast recordings
/// switching 5.1 to stereo, joined files). The resampler reads as many
/// planes as it was built for, so each change needs a new one.
#[derive(Default)]
struct Resampler {
    built: Option<(ffmpeg::software::resampling::Context, Shape)>,
    scratch: Vec<f32>,
}

impl Resampler {
    fn fits(&self, shape: Shape) -> bool {
        self.built
            .as_ref()
            .is_some_and(|(_, built)| *built == shape)
    }

    fn rebuild(&mut self, shape: Shape) -> Result<(), ffmpeg::Error> {
        self.built = None;
        let context = ffmpeg::software::resampling::Context::get(
            shape.0,
            shape.1,
            shape.2,
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
            ffmpeg::ChannelLayout::STEREO,
            AUDIO_RATE,
        )?;
        self.built = Some((context, shape));
        Ok(())
    }

    /// `frame`'s samples in the mix format. `frame` must fit.
    fn convert(&mut self, frame: &ffmpeg::frame::Audio) -> MediaResult<&[f32]> {
        let (context, _) = self.built.as_mut().expect("rebuilt for this frame");
        let capacity = unsafe {
            ffmpeg::ffi::swr_get_out_samples(context.as_mut_ptr(), frame.samples() as i32)
        }
        .max(0) as usize
            + 16;
        self.scratch.resize(capacity * 2, 0.0);
        let produced = unsafe {
            let out = [self.scratch.as_mut_ptr().cast::<u8>()];
            ffmpeg::ffi::swr_convert(
                context.as_mut_ptr(),
                out.as_ptr() as _,
                capacity as i32,
                (*frame.as_ptr()).extended_data as _,
                frame.samples() as i32,
            )
        };
        if produced < 0 {
            return Err("audio resample failed".to_owned());
        }
        Ok(&self.scratch[..produced as usize * 2])
    }
}

//! In-process audio playback and the transport's master clock.
//!
//! One mixer renders every audible timeline clip at 48 kHz stereo. Each clip
//! (a "voice") has its own decoder thread: ffmpeg-next decodes, swresample
//! converts, and pooled chunks tagged with their timeline sample travel to
//! the mixer over a bounded channel. The mixer runs in the output callback,
//! so the number of samples it has rendered is the playback clock. Picture
//! readiness gates that clock through a sample limit: rendering never passes
//! it, so a buffering cut holds audio and picture together.
//!
//! Outputs: the default sound device through cpal; a paced null output when
//! no device opens; and a manual output for tests and offline rendering,
//! which blocks for decoded data instead of rendering underrun silence.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg::Rescale;
use ffmpeg_next as ffmpeg;

use crate::MediaResult;

/// Mixer sample rate (frames per second). Every source resamples to it.
pub const AUDIO_RATE: u32 = 48_000;
const RATE: f64 = AUDIO_RATE as f64;
/// Decoded chunks a voice may queue ahead of the mixer (about one second
/// of AAC); the decoder blocks when the mixer has not consumed them.
const QUEUE_CHUNKS: usize = 48;
/// The manual output waits at most this long for decoded data.
const MANUAL_WAIT: Duration = Duration::from_secs(5);

/// One audible clip placed on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSegment {
    pub path: String,
    /// Timeline start and length, seconds.
    pub start: f64,
    pub duration: f64,
    /// Source time at `start`, seconds from the container origin.
    pub source_in: f64,
    pub volume: f32,
}

fn sample(seconds: f64) -> u64 {
    (seconds.max(0.0) * RATE).round() as u64
}

struct Chunk {
    /// Timeline sample of the first frame.
    at: u64,
    /// Interleaved stereo frames.
    data: Vec<f32>,
}

struct Voice {
    id: u64,
    start: u64,
    end: u64,
    volume: Arc<AtomicU32>,
    chunks: Receiver<MediaResult<Chunk>>,
    recycle: SyncSender<Vec<f32>>,
    current: Option<Chunk>,
    /// A chunk arrived, the decoder finished, or it failed.
    primed: bool,
    finished: bool,
}

impl Voice {
    fn covers(&self, at: u64) -> bool {
        self.start <= at && at < self.end
    }

    /// Take the next decoded chunk; `wait` blocks (manual output only).
    fn pull(&mut self, wait: bool, errors: &Mutex<Option<String>>) -> bool {
        let next = if wait {
            self.chunks.recv_timeout(MANUAL_WAIT).ok()
        } else {
            match self.chunks.try_recv() {
                Ok(chunk) => Some(chunk),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => None,
            }
        };
        self.primed = true;
        match next {
            Some(Ok(chunk)) => {
                if let Some(old) = self.current.replace(chunk) {
                    let _ = self.recycle.try_send(old.data);
                }
                true
            }
            Some(Err(error)) => {
                *errors.lock().unwrap() = Some(error);
                self.finished = true;
                false
            }
            None => {
                self.finished = true;
                false
            }
        }
    }

    /// Mix this voice into `out` (interleaved stereo) for timeline samples
    /// `[from, from + frames)`. Missing data is silence; later data for an
    /// already rendered sample is discarded, so the voice never drifts.
    fn mix(
        &mut self,
        out: &mut [f32],
        from: u64,
        frames: usize,
        wait: bool,
        errors: &Mutex<Option<String>>,
    ) {
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let end = (from + frames as u64).min(self.end);
        let mut at = from.max(self.start);
        while at < end {
            let chunk_end = self
                .current
                .as_ref()
                .map(|chunk| chunk.at + (chunk.data.len() / 2) as u64);
            match (chunk_end, &self.current) {
                (Some(chunk_end), Some(chunk)) if at < chunk_end => {
                    if at < chunk.at {
                        at = chunk.at.min(end);
                        continue;
                    }
                    let stop = chunk_end.min(end);
                    let offset = (at - chunk.at) as usize * 2;
                    let target = (at - from) as usize * 2;
                    let count = (stop - at) as usize * 2;
                    for (dst, src) in out[target..target + count]
                        .iter_mut()
                        .zip(&chunk.data[offset..offset + count])
                    {
                        *dst += src * volume;
                    }
                    at = stop;
                }
                _ => {
                    if self.finished || !self.pull(wait, errors) {
                        // Underrun or end of source: silence for this block.
                        return;
                    }
                }
            }
        }
    }
}

enum Command {
    Seek { generation: u64, at: u64 },
    Add(Box<Voice>),
    Remove(u64),
}

struct Shared {
    epoch: Instant,
    running: AtomicBool,
    /// Rendering never passes this timeline sample.
    limit: AtomicU64,
    /// Requested seek generation and position; the mixer applies them.
    seek_generation: AtomicU64,
    seek_at: AtomicU64,
    applied_generation: AtomicU64,
    /// Timeline sample at the start and end of the last rendered block,
    /// and when it was rendered (nanoseconds since `epoch`).
    block_start: AtomicU64,
    block_end: AtomicU64,
    block_nanos: AtomicU64,
    /// Device latency between rendering and hearing a sample.
    latency_nanos: AtomicU64,
    /// Latest reported clock, in thousandths of a sample, so the clock never
    /// steps back between seeks (a released hold is heard one queue later).
    reported: AtomicU64,
    errors: Mutex<Option<String>>,
}

impl Shared {
    fn now_nanos(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }
}

/// Owned by whichever thread renders: the device callback, the null pacer,
/// or the caller of `AudioEngine::render` (manual).
struct Mixer {
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    voices: Vec<Voice>,
    position: u64,
    generation: u64,
    stereo: Vec<f32>,
    manual: bool,
}

impl Mixer {
    /// Render `frames` stereo frames into `self.stereo` and advance the clock
    /// by as many as the limit, running state and voice priming allow.
    fn render(&mut self, frames: usize) -> usize {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Seek { generation, at } => {
                    self.voices.clear();
                    self.position = at;
                    self.generation = generation;
                }
                Command::Add(voice) => {
                    self.voices.retain(|existing| existing.id != voice.id);
                    self.voices.push(*voice);
                }
                Command::Remove(id) => self.voices.retain(|voice| voice.id != id),
            }
        }
        if self.stereo.len() < frames * 2 {
            self.stereo.resize(frames * 2, 0.0);
        }
        self.stereo[..frames * 2].fill(0.0);
        let shared = Arc::clone(&self.shared);
        let mut advance = 0;
        if shared.running.load(Ordering::Acquire) {
            // A voice under the clock with no data yet holds the clock, so
            // a starting clip is never heard late against its picture.
            let mut waiting = false;
            for voice in &mut self.voices {
                if voice.covers(self.position) && !voice.primed {
                    voice.pull(self.manual, &shared.errors);
                    waiting |= !voice.primed;
                }
            }
            if !waiting {
                let limit = shared.limit.load(Ordering::Acquire);
                advance = (limit.saturating_sub(self.position) as usize).min(frames);
            }
        }
        for voice in &mut self.voices {
            voice.mix(
                &mut self.stereo[..advance * 2],
                self.position,
                advance,
                self.manual,
                &shared.errors,
            );
        }
        let start = self.position;
        self.position += advance as u64;
        let position = self.position;
        self.voices.retain(|voice| voice.end > position);
        // A held (silent) block must not restart interpolation: the queue
        // ahead of it still drains towards the held position.
        if advance > 0 {
            shared.block_start.store(start, Ordering::Relaxed);
            shared
                .block_nanos
                .store(shared.now_nanos(), Ordering::Relaxed);
        }
        shared.block_end.store(self.position, Ordering::Relaxed);
        shared
            .applied_generation
            .store(self.generation, Ordering::Release);
        advance
    }

    fn write(&mut self, data: &mut [f32], channels: usize) {
        let frames = data.len() / channels.max(1);
        self.render(frames);
        for (frame, stereo) in data
            .chunks_exact_mut(channels)
            .zip(self.stereo.chunks_exact(2))
        {
            match channels {
                1 => frame[0] = (stereo[0] + stereo[1]) * 0.5,
                _ => {
                    frame[0] = stereo[0];
                    frame[1] = stereo[1];
                    frame[2..].fill(0.0);
                }
            }
        }
    }
}

enum Output {
    Device(#[allow(dead_code)] cpal::Stream),
    Null(Arc<AtomicBool>),
    Manual(Mixer),
}

struct VoiceHandle {
    segment: AudioSegment,
    volume: Arc<AtomicU32>,
}

/// The audio mixer and playback clock.
pub struct AudioEngine {
    shared: Arc<Shared>,
    commands: Sender<Command>,
    output: Output,
    voices: HashMap<u64, VoiceHandle>,
    /// What the output is: "device name", "no audio device", or "manual".
    pub description: String,
}

impl AudioEngine {
    fn parts() -> (Arc<Shared>, Sender<Command>, Mixer) {
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            running: AtomicBool::new(false),
            limit: AtomicU64::new(0),
            seek_generation: AtomicU64::new(0),
            seek_at: AtomicU64::new(0),
            applied_generation: AtomicU64::new(0),
            block_start: AtomicU64::new(0),
            block_end: AtomicU64::new(0),
            block_nanos: AtomicU64::new(0),
            latency_nanos: AtomicU64::new(0),
            reported: AtomicU64::new(0),
            errors: Mutex::new(None),
        });
        let (commands, receiver) = channel();
        let mixer = Mixer {
            shared: Arc::clone(&shared),
            commands: receiver,
            voices: Vec::new(),
            position: 0,
            generation: 0,
            stereo: vec![0.0; 8192 * 2],
            manual: false,
        };
        (shared, commands, mixer)
    }

    /// Play through the default sound device, or a paced null output when
    /// none opens (headless machines, `SDL_AUDIODRIVER=dummy` test runs).
    pub fn open() -> Self {
        let (shared, commands, mixer) = Self::parts();
        match open_device(mixer, &shared) {
            Ok((stream, name)) => Self {
                shared,
                commands,
                output: Output::Device(stream),
                voices: HashMap::new(),
                description: name,
            },
            Err((mixer, error)) => {
                let stop = spawn_null(mixer);
                Self {
                    shared,
                    commands,
                    output: Output::Null(stop),
                    voices: HashMap::new(),
                    description: format!("no audio device ({error})"),
                }
            }
        }
    }

    /// Paced in real time with no device: the clock without sound.
    pub fn null() -> Self {
        let (shared, commands, mixer) = Self::parts();
        Self {
            shared,
            commands,
            output: Output::Null(spawn_null(mixer)),
            voices: HashMap::new(),
            description: "no audio device".to_owned(),
        }
    }

    /// Renders only when `render` is called, waiting for decoded data
    /// instead of underrunning. Deterministic; for tests and offline use.
    pub fn manual() -> Self {
        let (shared, commands, mut mixer) = Self::parts();
        mixer.manual = true;
        Self {
            shared,
            commands,
            output: Output::Manual(mixer),
            voices: HashMap::new(),
            description: "manual".to_owned(),
        }
    }

    /// Manual output only: render `frames` frames and return the interleaved
    /// stereo mix of the frames the clock actually advanced over.
    pub fn render(&mut self, frames: usize) -> &[f32] {
        let Output::Manual(mixer) = &mut self.output else {
            return &[];
        };
        let advanced = mixer.render(frames);
        &mixer.stereo[..advanced * 2]
    }

    /// Manual output only: render `seconds` of timeline time (the clock may
    /// advance less when held).
    pub fn advance(&mut self, seconds: f64) {
        let target = self.position_sample() + sample(seconds);
        let mut guard = 0;
        while self.position_sample() < target && guard < 1_000_000 {
            let before = self.position_sample();
            let frames = (target - before).min(4096) as usize;
            if self.render(frames).is_empty() {
                break;
            }
            guard += 1;
        }
    }

    fn position_sample(&self) -> u64 {
        if self.shared.seek_generation.load(Ordering::Acquire)
            != self.shared.applied_generation.load(Ordering::Acquire)
        {
            return self.shared.seek_at.load(Ordering::Acquire);
        }
        self.shared.block_end.load(Ordering::Relaxed)
    }

    /// The playback clock in timeline seconds: the sample being heard now.
    pub fn clock(&self) -> f64 {
        if self.shared.seek_generation.load(Ordering::Acquire)
            != self.shared.applied_generation.load(Ordering::Acquire)
        {
            return self.shared.seek_at.load(Ordering::Acquire) as f64 / RATE;
        }
        let start = self.shared.block_start.load(Ordering::Relaxed) as f64;
        let end = self.shared.block_end.load(Ordering::Relaxed) as f64;
        // Nothing before the last seek is heard on this timeline.
        let floor = self.shared.seek_at.load(Ordering::Acquire) as f64;
        if matches!(self.output, Output::Manual(_)) {
            return end / RATE;
        }
        // Interpolate between blocks: the block rendered at `nanos` starts
        // being heard one device latency later.
        let since =
            self.shared
                .now_nanos()
                .saturating_sub(self.shared.block_nanos.load(Ordering::Relaxed)) as f64
                - self.shared.latency_nanos.load(Ordering::Relaxed) as f64;
        // Before the latency elapses this is still inside earlier blocks.
        let heard = start + since * 1e-9 * RATE;
        let heard = (heard.max(floor).min(end) * 1000.0) as u64;
        let reported = self.shared.reported.fetch_max(heard, Ordering::AcqRel);
        heard.max(reported) as f64 / 1000.0 / RATE
    }

    /// Device output latency last reported by the sound server.
    pub fn latency(&self) -> Duration {
        Duration::from_nanos(self.shared.latency_nanos.load(Ordering::Relaxed))
    }

    /// Move the clock to `time` and drop every voice.
    pub fn seek(&mut self, time: f64) {
        let generation = self.shared.seek_generation.load(Ordering::Acquire) + 1;
        let at = sample(time);
        self.shared.seek_at.store(at, Ordering::Release);
        self.shared.reported.store(at * 1000, Ordering::Release);
        self.shared
            .seek_generation
            .store(generation, Ordering::Release);
        self.voices.clear();
        let _ = self.commands.send(Command::Seek { generation, at });
    }

    pub fn set_running(&self, running: bool) {
        self.shared.running.store(running, Ordering::Release);
    }

    /// Rendering stops at this timeline time until it is raised.
    pub fn set_limit(&self, time: f64) {
        self.shared.limit.store(sample(time), Ordering::Release);
    }

    /// Make the voices exactly `segments` (keyed by clip id), starting new
    /// ones at the clock. Unchanged clips keep decoding; volume changes apply
    /// without restarting.
    pub fn set_voices(&mut self, segments: &[(u64, AudioSegment)]) {
        self.voices.retain(|id, handle| {
            let keep = segments
                .iter()
                .any(|(key, segment)| key == id && same_source(segment, &handle.segment));
            if !keep {
                let _ = self.commands.send(Command::Remove(*id));
            }
            keep
        });
        let from = self.position_sample();
        for (id, segment) in segments {
            if let Some(handle) = self.voices.get_mut(id) {
                handle
                    .volume
                    .store(segment.volume.to_bits(), Ordering::Relaxed);
                handle.segment.volume = segment.volume;
                continue;
            }
            let start = sample(segment.start);
            let end = sample(segment.start + segment.duration);
            if end <= from {
                continue;
            }
            let volume = Arc::new(AtomicU32::new(segment.volume.to_bits()));
            let (chunks_tx, chunks) = sync_channel(QUEUE_CHUNKS);
            let (recycle, recycle_rx) = sync_channel(QUEUE_CHUNKS + 2);
            let job = segment.clone();
            let first = from.max(start);
            thread::Builder::new()
                .name("eddie-audio-voice".to_owned())
                .spawn(move || {
                    if let Err(error) = decode_voice(&job, first, end, &chunks_tx, &recycle_rx) {
                        let _ = chunks_tx.send(Err(error));
                    }
                })
                .expect("spawn audio decoder");
            let _ = self.commands.send(Command::Add(Box::new(Voice {
                id: *id,
                start,
                end,
                volume: Arc::clone(&volume),
                chunks,
                recycle,
                current: None,
                primed: false,
                finished: false,
            })));
            self.voices.insert(
                *id,
                VoiceHandle {
                    segment: segment.clone(),
                    volume,
                },
            );
        }
    }

    /// Clip ids with a live voice.
    pub fn voice_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.voices.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// The last decode error, cleared on read.
    pub fn take_error(&self) -> Option<String> {
        self.shared.errors.lock().unwrap().take()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        if let Output::Null(stop) = &self.output {
            stop.store(true, Ordering::Release);
        }
    }
}

fn same_source(a: &AudioSegment, b: &AudioSegment) -> bool {
    a.path == b.path && a.start == b.start && a.duration == b.duration && a.source_in == b.source_in
}

fn spawn_null(mut mixer: Mixer) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    thread::Builder::new()
        .name("eddie-audio-null".to_owned())
        .spawn(move || {
            let started = Instant::now();
            let mut rendered = 0_u64;
            while !stopped.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
                let due = (started.elapsed().as_secs_f64() * RATE) as u64;
                while rendered < due {
                    let frames = (due - rendered).min(4096) as usize;
                    mixer.render(frames);
                    rendered += frames as u64;
                }
            }
        })
        .expect("spawn null audio output");
    stop
}

fn open_device(
    mixer: Mixer,
    shared: &Arc<Shared>,
) -> Result<(cpal::Stream, String), (Mixer, String)> {
    if std::env::var("SDL_AUDIODRIVER").as_deref() == Ok("dummy") {
        return Err((mixer, "dummy driver requested".to_owned()));
    }
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        return Err((mixer, "no default output".to_owned()));
    };
    let name = device.name().unwrap_or_else(|_| "audio device".to_owned());
    let config = match device.supported_output_configs() {
        Ok(configs) => configs
            .filter(|range| {
                range.sample_format() == cpal::SampleFormat::F32
                    && range.min_sample_rate().0 <= AUDIO_RATE
                    && AUDIO_RATE <= range.max_sample_rate().0
                    && (1..=8).contains(&range.channels())
            })
            .min_by_key(|range| (range.channels() as i32 - 2).abs())
            .map(|range| {
                range
                    .with_sample_rate(cpal::SampleRate(AUDIO_RATE))
                    .config()
            }),
        Err(error) => return Err((mixer, error.to_string())),
    };
    let Some(config) = config else {
        return Err((mixer, "no 48 kHz float output".to_owned()));
    };
    let channels = config.channels as usize;
    // The callback owns the mixer; a failed build hands it back through here.
    let slot = Arc::new(Mutex::new(Some(mixer)));
    let callback_slot = Arc::clone(&slot);
    let latency_shared = Arc::clone(shared);
    let mut owned: Option<Mixer> = None;
    // The first callback fills the empty device buffer.
    let mut buffer_frames = 0;
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let mixer = owned.get_or_insert_with(|| callback_slot.lock().unwrap().take().unwrap());
            let frames = data.len() / channels;
            buffer_frames = buffer_frames.max(frames);
            // PipeWire's ALSA plugin reports no delay. Then the queue ahead
            // of this block is the buffer minus the space just requested.
            let stamp = info.timestamp();
            let reported = stamp
                .playback
                .duration_since(&stamp.callback)
                .unwrap_or_default();
            let latency = if reported.is_zero() {
                Duration::from_secs_f64((buffer_frames - frames) as f64 / RATE)
            } else {
                reported
            };
            latency_shared
                .latency_nanos
                .store(latency.as_nanos() as u64, Ordering::Relaxed);
            mixer.write(data, channels);
        },
        |error| eprintln!("audio output: {error}"),
        None,
    );
    let error = match stream {
        Ok(stream) => match stream.play() {
            Ok(()) => return Ok((stream, name)),
            Err(error) => error.to_string(),
        },
        Err(error) => error.to_string(),
    };
    let returned = slot.lock().unwrap().take();
    match returned {
        Some(mixer) => Err((mixer, error)),
        // The callback ran before failing; start over without a device.
        None => {
            let (_, _, mixer) = AudioEngine::parts();
            Err((mixer, error))
        }
    }
}

/// Decode `segment` from timeline sample `first` to `end` into chunks.
fn decode_voice(
    segment: &AudioSegment,
    first: u64,
    end: u64,
    chunks: &SyncSender<MediaResult<Chunk>>,
    recycle: &Receiver<Vec<f32>>,
) -> MediaResult<()> {
    crate::decode::ensure_init();
    let fail = |context: &str, error: ffmpeg::Error| format!("{context}: {error}");
    let mut input = ffmpeg::format::input(&segment.path).map_err(|e| fail("open audio", e))?;
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
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(parameters)
        .map_err(|e| fail("audio parameters", e))?
        .decoder()
        .audio()
        .map_err(|e| fail("open audio decoder", e))?;

    let start = sample(segment.start);
    let source_start = sample(segment.source_in) as i64 - start as i64;
    // Source sample wanted at timeline sample `first`.
    let wanted = first as i64 + source_start;
    let seconds = wanted as f64 / RATE;
    let ts = origin
        .rescale(time_base, ffmpeg::rescale::TIME_BASE)
        .saturating_add(((seconds - 0.1).max(0.0) * ffmpeg::ffi::AV_TIME_BASE as f64) as i64);
    if seconds > 0.0 {
        input.seek(ts, ..ts).map_err(|e| fail("audio seek", e))?;
    }

    let mut resampler: Option<(ffmpeg::software::resampling::Context, u32)> = None;
    let mut frame = ffmpeg::frame::Audio::empty();
    // Source sample (at RATE) of the next resampled frame.
    let mut cursor: Option<i64> = None;
    let mut scratch: Vec<f32> = Vec::new();
    let mut eof = false;
    let mut packets = input.packets();
    loop {
        // Feed until a frame is ready.
        let got = decoder.receive_frame(&mut frame).is_ok();
        if !got {
            if eof {
                break;
            }
            match packets.next() {
                Some((stream, packet)) => {
                    if stream.index() == index {
                        let _ = decoder.send_packet(&packet);
                    }
                }
                None => {
                    let _ = decoder.send_eof();
                    eof = true;
                }
            }
            continue;
        }
        let rate = frame.rate();
        if rate == 0 || frame.samples() == 0 {
            continue;
        }
        if resampler.as_ref().is_none_or(|(_, r)| *r != rate) {
            let layout = if frame.channel_layout().channels() > 0 {
                frame.channel_layout()
            } else {
                ffmpeg::ChannelLayout::default(i32::from(frame.channels()))
            };
            let context = ffmpeg::software::resampling::Context::get(
                frame.format(),
                layout,
                rate,
                ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                ffmpeg::ChannelLayout::STEREO,
                AUDIO_RATE,
            )
            .map_err(|e| fail("audio resampler", e))?;
            resampler = Some((context, rate));
        }
        if let Some(pts) = frame.timestamp() {
            let source = pts.saturating_sub(origin) as f64 * f64::from(time_base.numerator())
                / f64::from(time_base.denominator());
            let at = (source * RATE).round() as i64;
            // Trust timestamps only across real discontinuities; resampled
            // frame counts drift by a sample against rounded timestamps.
            if cursor.is_none_or(|c| (c - at).abs() > (RATE * 0.02) as i64) {
                cursor = Some(at);
            }
        }
        let (context, _) = resampler.as_mut().unwrap();
        let capacity = unsafe {
            ffmpeg::ffi::swr_get_out_samples(context.as_mut_ptr(), frame.samples() as i32)
        }
        .max(0) as usize
            + 16;
        scratch.resize(capacity * 2, 0.0);
        let produced = unsafe {
            let out = [scratch.as_mut_ptr().cast::<u8>()];
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
        let produced = produced as usize;
        let block_source = cursor.unwrap_or(0);
        cursor = Some(block_source + produced as i64);
        // Timeline position of this block, then keep [first, end).
        let block_at = block_source - source_start;
        let keep_from = (first as i64 - block_at).clamp(0, produced as i64) as usize;
        let keep_to = (end as i64 - block_at).clamp(0, produced as i64) as usize;
        if keep_to > keep_from {
            let mut data = recycle.try_recv().unwrap_or_default();
            data.clear();
            data.extend_from_slice(&scratch[keep_from * 2..keep_to * 2]);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A device-like engine whose mixer the test renders by hand.
    fn device_like(latency: Duration) -> (AudioEngine, Mixer) {
        let (shared, commands, mixer) = AudioEngine::parts();
        shared
            .latency_nanos
            .store(latency.as_nanos() as u64, Ordering::Relaxed);
        let engine = AudioEngine {
            shared,
            commands,
            output: Output::Null(Arc::new(AtomicBool::new(true))),
            voices: HashMap::new(),
            description: "test device".to_owned(),
        };
        (engine, mixer)
    }

    #[test]
    fn held_clock_reaches_the_limit_once_the_queue_drains() {
        let (mut engine, mut mixer) = device_like(Duration::from_millis(57));
        engine.seek(0.0);
        engine.set_limit(0.1);
        engine.set_running(true);
        assert_eq!(mixer.render(4800), 4800);
        let mut last = engine.clock();
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(10));
            // Held: silent callbacks keep arriving.
            assert_eq!(mixer.render(480), 0);
            let clock = engine.clock();
            assert!(clock >= last, "clock went back: {last} -> {clock}");
            last = clock;
        }
        assert_eq!(engine.clock(), 0.1, "held clock never reached the limit");
        // Released audio is heard one queue later; the clock waits, not rewinds.
        engine.set_limit(1.0);
        assert_eq!(mixer.render(480), 480);
        assert_eq!(engine.clock(), 0.1);
    }

    #[test]
    fn clock_starts_at_the_seek_point_not_one_latency_before_it() {
        let (mut engine, mut mixer) = device_like(Duration::from_millis(57));
        engine.seek(2.0);
        engine.set_limit(10.0);
        mixer.render(480);
        assert_eq!(engine.clock(), 2.0);
        engine.set_running(true);
        mixer.render(480);
        assert!(engine.clock() >= 2.0, "{}", engine.clock());
    }
}

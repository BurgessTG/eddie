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
use std::sync::mpsc::{channel, sync_channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use decode::decode_voice;
use mixer::{Command, Mixer, Shared, Voice};
use output::{open_device, spawn_null, Output};

mod decode;
mod mixer;
mod output;

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
    /// Linear fade lengths, seconds; fitted with `openreel_engine::fit_fades`.
    pub fade_in: f64,
    pub fade_out: f64,
    /// Ducking ranges in clip-local seconds, from
    /// `openreel_engine::Timeline::duck_ranges`; empty when it never ducks.
    pub duck: Vec<(f64, f64)>,
    /// Voice cleanup (`voice.rs`), applied as the clip decodes.
    pub clean_voice: bool,
    /// Fades follow `openreel_engine::fade_shape`'s quarter sine.
    pub smooth_fades: bool,
}

/// Fitted fade lengths in samples, packed in/out so a fade change
/// applies without restarting the voice's decoder.
fn pack_fades(segment: &AudioSegment) -> u64 {
    let (fade_in, fade_out) =
        openreel_engine::fit_fades(segment.duration, segment.fade_in, segment.fade_out);
    (sample(fade_in).min(u64::from(u32::MAX)) << 32) | sample(fade_out).min(u64::from(u32::MAX))
}

fn sample(seconds: f64) -> u64 {
    (seconds.max(0.0) * RATE).round() as u64
}

struct VoiceHandle {
    segment: AudioSegment,
    volume: Arc<AtomicU32>,
    fades: Arc<AtomicU64>,
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
            master_gain: AtomicU32::new(1.0_f32.to_bits()),
            limiting: AtomicBool::new(false),
            reported: AtomicU64::new(0),
            errors: Mutex::new(None),
            device_lost: AtomicBool::new(false),
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
            envelope: 1.0,
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

    /// Play at the export's loudness: `Some(gain)` applies that linear gain
    /// and the 0.8 peak limiter; `None` plays the plain mix.
    pub fn set_loudness(&self, gain: Option<f32>) {
        self.shared
            .master_gain
            .store(gain.unwrap_or(1.0).to_bits(), Ordering::Relaxed);
        self.shared
            .limiting
            .store(gain.is_some(), Ordering::Relaxed);
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
                handle.fades.store(pack_fades(segment), Ordering::Relaxed);
                if handle.segment.duck != segment.duck {
                    handle.segment.duck = segment.duck.clone();
                    let _ = self
                        .commands
                        .send(Command::Duck(*id, segment.duck.as_slice().into()));
                }
                continue;
            }
            let start = sample(segment.start);
            let end = sample(segment.start + segment.duration);
            if end <= from {
                continue;
            }
            let volume = Arc::new(AtomicU32::new(segment.volume.to_bits()));
            let fades = Arc::new(AtomicU64::new(pack_fades(segment)));
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
                fades: Arc::clone(&fades),
                duck: segment.duck.as_slice().into(),
                smooth_fades: segment.smooth_fades,
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
                    fades,
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

    /// The sound device went away and this engine's clock has stopped for
    /// good. Open a new engine: it takes the default device there is now, or
    /// paces silently without one.
    pub fn device_lost(&self) -> bool {
        self.shared.device_lost.load(Ordering::Acquire)
    }

    /// Behave as if the device had just been unplugged (tests).
    #[doc(hidden)]
    pub fn lose_device(&self) {
        self.shared.device_lost.store(true, Ordering::Release);
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
    a.path == b.path
        && a.start == b.start
        && a.duration == b.duration
        && a.source_in == b.source_in
        && a.clean_voice == b.clean_voice
        && a.smooth_fades == b.smooth_fades
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

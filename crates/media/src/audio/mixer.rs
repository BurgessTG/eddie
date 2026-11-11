//! The mixer: voices pulled from their decode threads and summed into
//! the device block, which is also the transport clock.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::{MANUAL_WAIT, RATE};
use crate::MediaResult;

pub(super) struct Chunk {
    /// Timeline sample of the first frame.
    pub(super) at: u64,
    /// Interleaved stereo frames.
    pub(super) data: Vec<f32>,
}

pub(super) struct Voice {
    pub(super) id: u64,
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) volume: Arc<AtomicU32>,
    pub(super) fades: Arc<AtomicU64>,
    pub(super) duck: Arc<[(f64, f64)]>,
    pub(super) smooth_fades: bool,
    pub(super) chunks: Receiver<MediaResult<Chunk>>,
    pub(super) recycle: SyncSender<Vec<f32>>,
    pub(super) current: Option<Chunk>,
    /// A chunk arrived, the decoder finished, or it failed.
    pub(super) primed: bool,
    pub(super) finished: bool,
}

impl Voice {
    fn covers(&self, at: u64) -> bool {
        self.start <= at && at < self.end
    }

    /// Take the next decoded chunk; `wait` blocks (manual output only).
    fn pull(&mut self, wait: bool, errors: &Mutex<Option<String>>) -> bool {
        let next = if wait {
            match self.chunks.recv_timeout(MANUAL_WAIT) {
                Ok(chunk) => Some(chunk),
                Err(RecvTimeoutError::Disconnected) => None,
                // Rendering on would measure this clip as silence.
                Err(RecvTimeoutError::Timeout) => Some(Err(format!(
                    "audio decode produced nothing for {} s",
                    MANUAL_WAIT.as_secs()
                ))),
            }
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
        let fades = self.fades.load(Ordering::Relaxed);
        let (fade_in, fade_out) = (fades >> 32, fades & u64::from(u32::MAX));
        let (start, clip_end) = (self.start, self.end);
        // Shared, so pulling chunks below can borrow the voice mutably.
        let duck = Arc::clone(&self.duck);
        // Ramps over the fitted fades; unity in between.
        let smooth = self.smooth_fades;
        let gain = |at: u64| -> f32 {
            let mut gain = 1.0_f32;
            if fade_in > 0 && at < start + fade_in {
                let progress = (at - start) as f32 / fade_in as f32;
                gain = gain.min(openreel_engine::fade_shape(progress, smooth));
            }
            if fade_out > 0 && at + fade_out > clip_end {
                let progress = clip_end.saturating_sub(at) as f32 / fade_out as f32;
                gain = gain.min(openreel_engine::fade_shape(progress, smooth));
            }
            if !duck.is_empty() {
                gain *= openreel_engine::duck_gain((at - start) as f64 / RATE, &duck);
            }
            gain * volume
        };
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
                    if fade_in == 0 && fade_out == 0 && duck.is_empty() {
                        for (dst, src) in out[target..target + count]
                            .iter_mut()
                            .zip(&chunk.data[offset..offset + count])
                        {
                            *dst += src * volume;
                        }
                    } else {
                        for (index, (dst, src)) in out[target..target + count]
                            .as_chunks_mut::<2>()
                            .0
                            .iter_mut()
                            .zip(chunk.data[offset..offset + count].as_chunks::<2>().0)
                            .enumerate()
                        {
                            let g = gain(at + index as u64);
                            dst[0] += src[0] * g;
                            dst[1] += src[1] * g;
                        }
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

pub(super) enum Command {
    Seek { generation: u64, at: u64 },
    Add(Box<Voice>),
    Remove(u64),
    Duck(u64, Arc<[(f64, f64)]>),
}

pub(super) struct Shared {
    pub(super) epoch: Instant,
    pub(super) running: AtomicBool,
    /// Rendering never passes this timeline sample.
    pub(super) limit: AtomicU64,
    /// Requested seek generation and position; the mixer applies them.
    pub(super) seek_generation: AtomicU64,
    pub(super) seek_at: AtomicU64,
    pub(super) applied_generation: AtomicU64,
    /// Timeline sample at the start and end of the last rendered block,
    /// and when it was rendered (nanoseconds since `epoch`).
    pub(super) block_start: AtomicU64,
    pub(super) block_end: AtomicU64,
    pub(super) block_nanos: AtomicU64,
    /// Device latency between rendering and hearing a sample.
    pub(super) latency_nanos: AtomicU64,
    /// Master gain (f32 bits) and whether the peak limiter runs; see
    /// `AudioEngine::set_loudness`.
    pub(super) master_gain: AtomicU32,
    pub(super) limiting: AtomicBool,
    /// Latest reported clock, in thousandths of a sample, so the clock never
    /// steps back between seeks (a released hold is heard one queue later).
    pub(super) reported: AtomicU64,
    pub(super) errors: Mutex<Option<String>>,
    /// The device stopped calling back for good (unplugged, or its sound
    /// server went away), so this engine's clock will never move again.
    pub(super) device_lost: AtomicBool,
}

impl Shared {
    pub(super) fn now_nanos(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }
}

/// Owned by whichever thread renders: the device callback, the null pacer,
/// or the caller of `AudioEngine::render` (manual).
pub(super) struct Mixer {
    pub(super) shared: Arc<Shared>,
    pub(super) commands: Receiver<Command>,
    pub(super) voices: Vec<Voice>,
    pub(super) position: u64,
    pub(super) generation: u64,
    pub(super) stereo: Vec<f32>,
    pub(super) manual: bool,
    /// Limiter gain envelope, 1 when idle.
    pub(super) envelope: f32,
}

impl Mixer {
    /// Render `frames` stereo frames into `self.stereo` and advance the clock
    /// by as many as the limit, running state and voice priming allow.
    pub(super) fn render(&mut self, frames: usize) -> usize {
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
                Command::Duck(id, duck) => {
                    if let Some(voice) = self.voices.iter_mut().find(|voice| voice.id == id) {
                        voice.duck = duck;
                    }
                }
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
        self.master(advance);
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

    /// The export's loudness stage: one fixed gain, then a peak limiter at
    /// `LIMIT` with instant attack and a 50 ms release. Export's `alimiter`
    /// looks ahead 5 ms; the preview cannot without delaying its clock, so
    /// a sudden peak is caught on its first sample instead of eased in.
    fn master(&mut self, frames: usize) {
        const LIMIT: f32 = 0.8;
        let gain = f32::from_bits(self.shared.master_gain.load(Ordering::Relaxed));
        let limiting = self.shared.limiting.load(Ordering::Relaxed);
        if gain == 1.0 && !limiting {
            return;
        }
        let release = 1.0 - (-1.0 / (0.05 * RATE as f32)).exp();
        for frame in self.stereo[..frames * 2].as_chunks_mut::<2>().0 {
            frame[0] *= gain;
            frame[1] *= gain;
            if limiting {
                let peak = frame[0].abs().max(frame[1].abs());
                let target = if peak > LIMIT { LIMIT / peak } else { 1.0 };
                self.envelope = (self.envelope + (1.0 - self.envelope) * release).min(target);
                frame[0] *= self.envelope;
                frame[1] *= self.envelope;
            }
        }
    }

    pub(super) fn write(&mut self, data: &mut [f32], channels: usize) {
        let frames = data.len() / channels.max(1);
        self.render(frames);
        for (frame, stereo) in data
            .chunks_exact_mut(channels)
            .zip(self.stereo.as_chunks::<2>().0)
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

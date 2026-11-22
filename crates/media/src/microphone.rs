//! Recording from a microphone: the system's default input through cpal,
//! metered while it is open and written to a WAV while a take runs.
//!
//! The device callback mixes each block to mono and hands it to a writer
//! thread, which writes the file and the take's waveform peaks, so the
//! callback never waits on the disk. Tests and machines without a sound
//! device use a manual microphone that captures a tone when told to.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use crate::{AUDIO_RATE, WAVEFORM_PEAKS_PER_SEC};
use wav::Wav;

mod wav;

/// Blocks a take may queue for its writer, about ten seconds of audio:
/// past that the writer has stalled and blocks are dropped and counted.
const QUEUE_BLOCKS: usize = 1024;
/// The manual microphone's tone.
const TONE_HZ: f64 = 440.0;
pub const TONE_LEVEL: f32 = 0.25;

/// An open input: the level of what it hears, and a take while one runs.
pub struct Microphone {
    input: Input,
    shared: Arc<Shared>,
    /// What it records from, for the window to show.
    pub name: String,
    /// Samples a second it captures, and so of every take.
    pub rate: u32,
}

enum Input {
    Device(#[allow(dead_code)] cpal::Stream),
    /// A tone, captured by [`Microphone::advance`]. `owed` carries the
    /// part of a sample the last call did not make.
    Manual {
        phase: f64,
        owed: f64,
    },
}

struct Shared {
    /// The loudest sample since the level was last read, as `f32` bits
    /// (which order like the numbers for those from 0 up).
    peak: AtomicU32,
    /// Where captured blocks go while a take runs.
    take: Mutex<Option<SyncSender<Block>>>,
    /// Samples the writer fell too far behind to take.
    dropped: AtomicU64,
    error: Mutex<Option<String>>,
}

/// Captured samples, mono, and when the first of them was captured.
struct Block {
    samples: Vec<f32>,
    captured: Option<Instant>,
}

impl Shared {
    /// Meter a block of mono samples and hand it to the take, if one runs.
    /// The block is only collected for a take, so metering allocates
    /// nothing.
    fn capture(&self, samples: impl Iterator<Item = f32> + Clone, captured: Option<Instant>) {
        let peak = samples.clone().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        // Only a take starting or stopping holds the lock; that block is
        // before the take or after it.
        let Ok(take) = self.take.try_lock() else {
            return;
        };
        if let Some(sender) = take.as_ref() {
            let samples: Vec<f32> = samples.collect();
            let len = samples.len() as u64;
            if let Err(TrySendError::Full(_)) = sender.try_send(Block { samples, captured }) {
                self.dropped.fetch_add(len, Ordering::Relaxed);
            }
        }
    }
}

impl Microphone {
    /// The system's default input: on Linux whatever the sound server
    /// records from, on a Mac the input chosen in System Settings.
    pub fn open() -> Result<Self, String> {
        if std::env::var("SDL_AUDIODRIVER").as_deref() == Ok("dummy") {
            return Err("no sound device (dummy driver requested)".to_owned());
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no microphone found".to_owned())?;
        // Linux names its default after the ALSA plugin, which says nothing
        // about the microphone; a Mac names the device.
        let name = if cfg!(target_os = "macos") {
            device
                .description()
                .map_or_else(|_| "Microphone".to_owned(), |d| d.name().to_owned())
        } else {
            "System default".to_owned()
        };
        let config = input_config(&device)?;
        let rate = config.sample_rate();
        let channels = usize::from(config.channels());
        let shared = Arc::new(Shared {
            peak: AtomicU32::new(0),
            take: Mutex::new(None),
            dropped: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => stream::<f32>(&device, &config, channels, &shared),
            cpal::SampleFormat::I16 => stream::<i16>(&device, &config, channels, &shared),
            cpal::SampleFormat::I32 => stream::<i32>(&device, &config, channels, &shared),
            cpal::SampleFormat::U16 => stream::<u16>(&device, &config, channels, &shared),
            cpal::SampleFormat::U8 => stream::<u8>(&device, &config, channels, &shared),
            other => return Err(format!("the microphone sends {other} samples")),
        }?;
        stream.play().map_err(|error| error.to_string())?;
        Ok(Self {
            input: Input::Device(stream),
            shared,
            name,
            rate,
        })
    }

    /// A microphone that hears a 440 Hz tone at [`TONE_LEVEL`], one call
    /// of [`Self::advance`] at a time.
    pub fn manual() -> Self {
        Self {
            input: Input::Manual {
                phase: 0.0,
                owed: 0.0,
            },
            shared: Arc::new(Shared {
                peak: AtomicU32::new(0),
                take: Mutex::new(None),
                dropped: AtomicU64::new(0),
                error: Mutex::new(None),
            }),
            name: "Test tone".to_owned(),
            rate: AUDIO_RATE,
        }
    }

    /// Manual microphone only: capture `seconds` of its tone.
    pub fn advance(&mut self, seconds: f64) {
        let Input::Manual { phase, owed } = &mut self.input else {
            return;
        };
        let wanted = *owed + seconds.max(0.0) * f64::from(self.rate);
        let frames = wanted.floor();
        *owed = wanted - frames;
        let step = std::f64::consts::TAU * TONE_HZ / f64::from(self.rate);
        let start = *phase;
        *phase = (start + step * frames) % std::f64::consts::TAU;
        let samples = (0..frames as usize).map(move |i| {
            ((start + step * i as f64) % std::f64::consts::TAU).sin() as f32 * TONE_LEVEL
        });
        self.shared.capture(samples, None);
    }

    /// The loudest sample heard since the last call, from 0 to 1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.peak.swap(0, Ordering::Relaxed))
    }

    /// Why the device stopped, once.
    pub fn take_error(&self) -> Option<String> {
        self.shared.error.lock().ok()?.take()
    }

    /// Start writing what it hears to a new WAV at `path`.
    pub fn record(&self, path: &Path) -> Result<Take, String> {
        let mut armed = self.shared.take.lock().map_err(|_| "microphone failed")?;
        if armed.is_some() {
            return Err("a take is already recording".to_owned());
        }
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|error| format!("couldn't make {}: {error}", folder.display()))?;
        }
        let wav = Wav::create(path, self.rate)
            .map_err(|error| format!("couldn't write {}: {error}", path.display()))?;
        let (sender, blocks) = sync_channel(QUEUE_BLOCKS);
        let progress = Arc::new(Progress::default());
        let writing = Arc::clone(&progress);
        let rate = self.rate;
        let writer = std::thread::Builder::new()
            .name("voiceover".to_owned())
            .spawn(move || write(blocks, wav, rate, &writing))
            .map_err(|error| error.to_string())?;
        self.shared.dropped.store(0, Ordering::Relaxed);
        *armed = Some(sender);
        Ok(Take {
            path: path.to_owned(),
            rate,
            shared: Arc::clone(&self.shared),
            progress,
            writer: Some(writer),
        })
    }
}

/// A 48 kHz configuration when the device has one, else its own; float
/// samples first, then as few channels as it offers.
fn input_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, String> {
    let rank = |format: cpal::SampleFormat| match format {
        cpal::SampleFormat::F32 => 0,
        cpal::SampleFormat::I16 => 1,
        _ => 2,
    };
    let at_48k = device.supported_input_configs().ok().and_then(|configs| {
        configs
            .filter(|range| {
                range.min_sample_rate() <= AUDIO_RATE && AUDIO_RATE <= range.max_sample_rate()
            })
            .min_by_key(|range| (rank(range.sample_format()), range.channels()))
            .map(|range| range.with_sample_rate(AUDIO_RATE))
    });
    match at_48k {
        Some(config) => Ok(config),
        None => device
            .default_input_config()
            .map_err(|error| error.to_string()),
    }
}

fn stream<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    channels: usize,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let capturing = Arc::clone(shared);
    let failing = Arc::clone(shared);
    device
        .build_input_stream(
            config.config(),
            move |data: &[T], info: &cpal::InputCallbackInfo| {
                // When the block's first sample was captured, on our clock.
                let stamp = info.timestamp();
                let waited = stamp.callback.duration_since(stamp.capture);
                let captured = Instant::now().checked_sub(waited);
                let mono = data.chunks_exact(channels.max(1)).map(|frame| {
                    frame
                        .iter()
                        .map(|&s| <f32 as FromSample<T>>::from_sample_(s))
                        .sum::<f32>()
                        / frame.len() as f32
                });
                capturing.capture(mono, captured);
            },
            move |error| {
                if let Ok(mut slot) = failing.error.lock() {
                    *slot = Some(error.to_string());
                }
            },
            None,
        )
        .map_err(|error| error.to_string())
}

/// What a take's writer reports as it goes.
#[derive(Default)]
struct Progress {
    frames: AtomicU64,
    /// The loudest sample of each 1/50 s, as imports read them.
    peaks: Mutex<Vec<f32>>,
    /// When its first sample was captured, if the device said.
    first: Mutex<Option<Instant>>,
}

fn write(
    blocks: Receiver<Block>,
    mut wav: Wav,
    rate: u32,
    progress: &Progress,
) -> Result<u64, String> {
    let per_peak = (rate / WAVEFORM_PEAKS_PER_SEC).max(1);
    let (mut loudest, mut counted) = (0.0_f32, 0);
    let mut first = true;
    let mut peaks = Vec::new();
    while let Ok(block) = blocks.recv() {
        if first {
            first = false;
            *progress.first.lock().map_err(|_| "take failed")? = block.captured;
        }
        wav.write(&block.samples)
            .map_err(|error| format!("couldn't write the take: {error}"))?;
        for sample in &block.samples {
            loudest = loudest.max(sample.abs());
            counted += 1;
            if counted == per_peak {
                peaks.push(loudest.min(1.0));
                (loudest, counted) = (0.0, 0);
            }
        }
        if !peaks.is_empty() {
            progress
                .peaks
                .lock()
                .map_err(|_| "take failed")?
                .append(&mut peaks);
        }
        progress
            .frames
            .fetch_add(block.samples.len() as u64, Ordering::Relaxed);
    }
    wav.finish()
        .map_err(|error| format!("couldn't finish the take: {error}"))
}

/// A recording in progress.
pub struct Take {
    path: PathBuf,
    rate: u32,
    shared: Arc<Shared>,
    progress: Arc<Progress>,
    writer: Option<JoinHandle<Result<u64, String>>>,
}

/// A finished take.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub path: PathBuf,
    pub seconds: f64,
    /// When its first sample was captured; the manual microphone does not
    /// say.
    pub first_sample: Option<Instant>,
    /// Seconds of sound lost because the disk fell behind.
    pub dropped: f64,
}

impl Take {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How much has been written.
    pub fn seconds(&self) -> f64 {
        self.progress.frames.load(Ordering::Relaxed) as f64 / f64::from(self.rate)
    }

    /// The waveform so far, [`WAVEFORM_PEAKS_PER_SEC`] peaks a second.
    pub fn peaks(&self) -> MutexGuard<'_, Vec<f32>> {
        self.progress
            .peaks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Stop recording and finish the file.
    pub fn finish(mut self) -> Result<Recorded, String> {
        self.disarm();
        let written = self
            .writer
            .take()
            .ok_or("take already finished")?
            .join()
            .map_err(|_| "the take's writer failed".to_owned())??;
        let first_sample = *self
            .progress
            .first
            .lock()
            .map_err(|_| "take failed".to_owned())?;
        let rate = f64::from(self.rate);
        Ok(Recorded {
            path: self.path.clone(),
            seconds: written as f64 / rate,
            first_sample,
            dropped: self.shared.dropped.load(Ordering::Relaxed) as f64 / rate,
        })
    }

    fn disarm(&self) {
        if let Ok(mut armed) = self.shared.take.lock() {
            *armed = None;
        }
    }
}

impl Drop for Take {
    /// A take dropped unfinished stops; its writer finishes the file alone.
    fn drop(&mut self) {
        self.disarm();
    }
}

#[cfg(test)]
mod tests;

//! Where the mix goes: a sound device through cpal, or a paced null
//! output when there is none.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::mixer::{Mixer, Shared};
use super::RATE;
use crate::{AudioEngine, AUDIO_RATE};

pub(super) enum Output {
    Device(#[allow(dead_code)] cpal::Stream),
    Null(Arc<AtomicBool>),
    Manual(Mixer),
}

pub(super) fn spawn_null(mut mixer: Mixer) -> Arc<AtomicBool> {
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

pub(super) fn open_device(
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
    let name = device
        .description()
        .map_or_else(|_| "audio device".to_owned(), |d| d.name().to_owned());
    let config = match device.supported_output_configs() {
        Ok(configs) => configs
            .filter(|range| {
                range.sample_format() == cpal::SampleFormat::F32
                    && range.min_sample_rate() <= AUDIO_RATE
                    && AUDIO_RATE <= range.max_sample_rate()
                    && (1..=8).contains(&range.channels())
            })
            .min_by_key(|range| (range.channels() as i32 - 2).abs())
            .map(|range| range.with_sample_rate(AUDIO_RATE).config()),
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
    let error_shared = Arc::clone(shared);
    let mut owned: Option<Mixer> = None;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let mixer = owned.get_or_insert_with(|| callback_slot.lock().unwrap().take().unwrap());
            // ALSA's delay: everything queued ahead of this block, including
            // PipeWire's graph quantum. (cpal <= 0.16 read it through alsa
            // 0.9.1, whose status struct reads as zero in release builds.)
            let stamp = info.timestamp();
            let latency = stamp.playback.duration_since(stamp.callback);
            latency_shared
                .latency_nanos
                .store(latency.as_nanos() as u64, Ordering::Relaxed);
            mixer.write(data, channels);
        },
        move |error| {
            eprintln!("audio output: {error}");
            // cpal's worker returns after these two; an underrun or a
            // rerouted default device keeps calling back.
            if matches!(
                error.kind(),
                cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable
            ) {
                error_shared.device_lost.store(true, Ordering::Release);
            }
        },
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

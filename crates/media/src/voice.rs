//! One-click voice cleanup: an 80 Hz high-pass for rumble and handling
//! noise, then a gentle compressor that evens out phone and lapel speech.
//! Export runs FFmpeg's `highpass` and `acompressor`; the preview runs the
//! same two algorithms here so what plays is what exports: the RBJ cookbook
//! biquad (as in fframes' `audio_effects.rs`, MIT) and a port of
//! `acompressor`'s peak detector, Hermite soft knee and gain law
//! (`libavfilter/af_sidechaincompress.c`).

use std::f64::consts::TAU;

const HIGHPASS_HZ: f64 = 80.0;
const THRESHOLD_DB: f64 = -24.0;
const RATIO: f64 = 3.0;
/// Milliseconds, in `acompressor`'s units (its level follows a quarter of them).
const ATTACK_MS: f64 = 10.0;
const RELEASE_MS: f64 = 150.0;
/// The knee spans this factor around the threshold (2 is 6 dB in all).
const KNEE: f64 = 2.0;
const MAKEUP_DB: f64 = 4.0;

fn linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The export's filter chain for a cleaned clip, ahead of volume and fades.
pub(crate) fn voice_filter() -> String {
    format!(
        ",highpass=f={HIGHPASS_HZ}:width_type=q:width=0.707,acompressor=threshold={:.8}:\
         ratio={RATIO}:attack={ATTACK_MS}:release={RELEASE_MS}:knee={KNEE}:makeup={:.8}:\
         detection=peak:link=maximum",
        linear(THRESHOLD_DB),
        linear(MAKEUP_DB),
    )
}

/// Streaming cleanup state for one interleaved stereo voice.
pub(crate) struct VoiceCleanup {
    b: [f64; 3],
    a: [f64; 2],
    /// Per channel: last two inputs and outputs.
    history: [[f64; 4]; 2],
    attack: f64,
    release: f64,
    /// The compressor's detected level (linear peak follower).
    level: f64,
    /// Scales the detector to the source's own level (√2 for upmixed mono).
    detector_gain: f64,
}

impl VoiceCleanup {
    pub(crate) fn new(rate: f64) -> Self {
        let w = TAU * HIGHPASS_HZ / rate;
        let (cos, alpha) = (w.cos(), w.sin() / (2.0 * 0.707));
        let a0 = 1.0 + alpha;
        let b = (1.0 + cos) / 2.0;
        let coefficient = |ms: f64| (1.0 / (ms * rate / 4000.0)).min(1.0);
        Self {
            b: [b / a0, -2.0 * b / a0, b / a0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            history: [[0.0; 4]; 2],
            attack: coefficient(ATTACK_MS),
            release: coefficient(RELEASE_MS),
            level: 0.0,
            detector_gain: 1.0,
        }
    }

    /// The voice is a mono source upmixed 3 dB down per channel.
    pub(crate) fn set_mono(&mut self, mono: bool) {
        self.detector_gain = if mono { std::f64::consts::SQRT_2 } else { 1.0 };
    }

    /// Clean interleaved stereo frames in place.
    pub(crate) fn process(&mut self, frames: &mut [f32]) {
        let makeup = linear(MAKEUP_DB);
        let knee_start = linear(THRESHOLD_DB) / KNEE.sqrt();
        for frame in frames.chunks_exact_mut(2) {
            let mut filtered = [0.0_f64; 2];
            for ((sample, h), out) in frame
                .iter()
                .zip(self.history.iter_mut())
                .zip(filtered.iter_mut())
            {
                let x = f64::from(*sample);
                let y = self.b[0] * x + self.b[1] * h[0] + self.b[2] * h[1]
                    - self.a[0] * h[2]
                    - self.a[1] * h[3];
                *h = [x, h[0], y, h[2]];
                *out = y;
            }
            let peak = filtered[0].abs().max(filtered[1].abs()) * self.detector_gain;
            let coefficient = if peak > self.level {
                self.attack
            } else {
                self.release
            };
            self.level += (peak - self.level) * coefficient;
            let gain = if self.level > knee_start {
                compressor_gain(self.level)
            } else {
                1.0
            };
            frame[0] = (filtered[0] * gain * makeup) as f32;
            frame[1] = (filtered[1] * gain * makeup) as f32;
        }
    }
}

/// `acompressor`'s downward gain for a detected linear level, in the log
/// domain with a cubic Hermite knee.
fn compressor_gain(level: f64) -> f64 {
    let threshold = linear(THRESHOLD_DB).ln();
    let knee_start = threshold - KNEE.sqrt().ln();
    let knee_stop = threshold + KNEE.sqrt().ln();
    let compressed_knee_stop = (knee_stop - threshold) / RATIO + threshold;
    let slope = level.ln();
    let gain = if slope < knee_stop {
        hermite(
            slope,
            knee_start,
            knee_stop,
            knee_start,
            compressed_knee_stop,
            1.0,
            1.0 / RATIO,
        )
    } else {
        (slope - threshold) / RATIO + threshold
    };
    (gain - slope).exp()
}

fn hermite(x: f64, x0: f64, x1: f64, p0: f64, p1: f64, m0: f64, m1: f64) -> f64 {
    let width = x1 - x0;
    let t = (x - x0) / width;
    let (m0, m1) = (m0 * width, m1 * width);
    let c2 = -3.0 * p0 - 2.0 * m0 + 3.0 * p1 - m1;
    let c3 = 2.0 * p0 + m0 - 2.0 * p1 + m1;
    c3 * t * t * t + c2 * t * t + m0 * t + p0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, amplitude: f32, seconds: f64) -> Vec<f32> {
        (0..(48_000.0 * seconds) as usize)
            .flat_map(|i| {
                let s = amplitude * (TAU * hz * i as f64 / 48_000.0).sin() as f32;
                [s, s]
            })
            .collect()
    }

    fn rms_db(frames: &[f32]) -> f64 {
        let tail = &frames[frames.len() / 2..];
        let mean = tail.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / tail.len() as f64;
        10.0 * mean.log10()
    }

    #[test]
    fn rumble_drops_and_loud_speech_is_evened_out() {
        // A quiet 1 kHz tone below the threshold only gains the makeup.
        let mut quiet = tone(1000.0, 0.02, 1.0);
        let before = rms_db(&quiet);
        VoiceCleanup::new(48_000.0).process(&mut quiet);
        assert!((rms_db(&quiet) - before - MAKEUP_DB).abs() < 0.2);

        // 30 Hz rumble at the same level loses well over 12 dB.
        let mut rumble = tone(30.0, 0.02, 1.0);
        VoiceCleanup::new(48_000.0).process(&mut rumble);
        assert!(rms_db(&rumble) < before + MAKEUP_DB - 12.0);

        // A loud tone 12 dB over the threshold: 3:1 leaves its peak 4 dB over,
        // an 8 dB cut; the level follower sits a little under the peak, so a
        // little less. Export's acompressor test checks the exact curve.
        let peak_db = THRESHOLD_DB + 12.0;
        let mut loud = tone(1000.0, 10f64.powf(peak_db / 20.0) as f32, 1.0);
        let before = rms_db(&loud);
        VoiceCleanup::new(48_000.0).process(&mut loud);
        let cut = MAKEUP_DB - (rms_db(&loud) - before);
        assert!((6.0..=8.0).contains(&cut), "cut {cut:.2} dB");
    }
}

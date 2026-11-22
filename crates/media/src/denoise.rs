//! Noise reduction: FFmpeg's `afftdn`, told the noise of the recording it
//! cleans. Its own noise tracking barely moved (2 dB off white noise in
//! tests), and its default floor of -50 dB does nothing for noise above it,
//! so [`measure_noise`] reads a source's quietest stretches into a
//! [`NoiseProfile`]: the level and the shape over `afftdn`'s 15 bands.
//! Export puts [`denoise_filter`] in its graph and the preview runs the
//! same chain in process (`chain::Chain`).

use serde::{Deserialize, Serialize};

use crate::{MediaResult, AUDIO_RATE};

/// `afftdn`'s profile bands (`band_noise`).
pub const NOISE_BANDS: usize = 15;

/// What a recording's background noise sounds like: `floor` is its level
/// in dBFS, as a white noise of that level would be, and `bands` how far
/// each of `afftdn`'s bands sits above or below it, in dB.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseProfile {
    pub floor: f32,
    pub bands: [f32; NOISE_BANDS],
}

/// Samples `afftdn` holds back at the mix rate: its window is three hops of
/// a rate/80 and the first hop out is the window less one hop
/// (`af_afftdn.c`). Its output keeps the input's length, so a clip reads
/// this much further and drops this much from the front.
pub(crate) const DENOISE_DELAY: usize = 2 * AUDIO_RATE as usize / 80;

/// Source seconds read past a denoised clip's end ([`DENOISE_DELAY`]).
pub(crate) const DENOISE_TAIL: f64 = DENOISE_DELAY as f64 / AUDIO_RATE as f64;

/// How far the noise is brought down, in dB. 12 (the default) took 9.6 dB
/// of hiss away between syllables in tests, 18 took 12 with the voice no
/// worse; past that, gains flutter more for little.
const REDUCTION_DB: u32 = 18;

/// The `afftdn` chain that reduces `profile` and puts the sound back in
/// step: its output starts [`DENOISE_DELAY`] late.
pub(crate) fn denoise_filter(profile: &NoiseProfile) -> String {
    let bands: Vec<String> = profile
        .bands
        .iter()
        .map(|band| format!("{band:.2}"))
        .collect();
    format!(
        "afftdn=nr={REDUCTION_DB}:nf={:.2}:nt=custom:bn={},\
         atrim=start_sample={DENOISE_DELAY},asetpts=PTS-STARTPTS",
        profile.floor,
        bands.join("|")
    )
}

/// Samples per analysed frame (about 43 ms at the mix rate).
const FRAME: usize = 2048;
/// Sound read from the start of a file; ten minutes describe its noise.
const MEASURE_SECONDS: f64 = 600.0;
/// The share of frames, quietest first, taken as the noise.
const QUIET_SHARE: f64 = 0.15;
/// Frames under this power (-120 dBFS) are digital silence, not noise.
const SILENT_POWER: f64 = 1e-12;

/// `afftdn`'s band centres in Hz: 80, then each 1.5 times the last plus 5,
/// rounded as `af_afftdn.c` rounds them.
fn band_centres() -> [f64; NOISE_BANDS] {
    let mut centres = [80_i64; NOISE_BANDS];
    for i in 1..NOISE_BANDS {
        let c = (1.5 * centres[i - 1] as f64 + 5.0).round() as i64;
        centres[i] = if c < 1000 {
            10 * (c / 10)
        } else if c < 5000 {
            50 * ((c + 20) / 50)
        } else if c < 15000 {
            100 * ((c + 45) / 100)
        } else {
            1000 * ((c + 495) / 1000)
        };
    }
    centres.map(|c| c as f64)
}

/// The background noise of the sound at `path`, from its first ten minutes.
pub fn measure_noise(path: &str) -> MediaResult<NoiseProfile> {
    let mut analysis = Analysis::new();
    crate::audio::read_stereo(path, MEASURE_SECONDS, |samples, mono| {
        analysis.push(samples, mono)
    })?;
    Ok(analysis.profile())
}

/// Per-frame power and band powers of interleaved stereo at the mix rate.
struct Analysis {
    fft: Fft,
    window: Vec<f32>,
    window_power: f64,
    /// Each band's bins: `[first, last]`.
    bins: [(usize, usize); NOISE_BANDS],
    pending: Vec<f32>,
    /// Per frame: mean power per channel, then each band's mean bin power.
    frames: Vec<(f64, [f64; NOISE_BANDS])>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Analysis {
    fn new() -> Self {
        let window: Vec<f32> = (0..FRAME)
            .map(|i| {
                let s = (std::f64::consts::PI * i as f64 / FRAME as f64).sin();
                (s * s) as f32
            })
            .collect();
        let window_power = window.iter().map(|w| f64::from(*w).powi(2)).sum();
        let hz = f64::from(AUDIO_RATE) / FRAME as f64;
        let bins = band_centres().map(|centre| {
            let low = (centre / 1.224_745 / hz).ceil() as usize;
            let high = ((centre * 1.224_745 / hz).floor() as usize).min(FRAME / 2);
            if low <= high {
                (low, high)
            } else {
                let nearest = ((centre / hz).round() as usize).min(FRAME / 2);
                (nearest, nearest)
            }
        });
        Self {
            fft: Fft::new(FRAME),
            window,
            window_power,
            bins,
            pending: Vec::with_capacity(FRAME * 2),
            frames: Vec::new(),
            re: vec![0.0; FRAME],
            im: vec![0.0; FRAME],
        }
    }

    /// Interleaved stereo; `mono` when the source is mono, which the mix
    /// format spreads 3 dB down over both channels.
    fn push(&mut self, mut samples: &[f32], mono: bool) {
        while !samples.is_empty() {
            let take = (FRAME * 2 - self.pending.len()).min(samples.len());
            self.pending.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.pending.len() == FRAME * 2 {
                self.frame(mono);
                self.pending.clear();
            }
        }
    }

    fn frame(&mut self, mono: bool) {
        // A mono source's power is the two channels' sum; a stereo one's
        // their mean. Both are the level one channel of `afftdn` sees.
        let channels = if mono { 1.0 } else { 2.0 };
        let power = self
            .pending
            .iter()
            .map(|s| f64::from(*s).powi(2))
            .sum::<f64>()
            / (FRAME as f64 * channels);
        // Both channels in one transform: left real, right imaginary.
        for (i, pair) in self.pending.as_chunks::<2>().0.iter().enumerate() {
            self.re[i] = pair[0] * self.window[i];
            self.im[i] = pair[1] * self.window[i];
        }
        self.fft.run(&mut self.re, &mut self.im);
        // |L(k)|^2 + |R(k)|^2 = (|Z(k)|^2 + |Z(N-k)|^2) / 2; a white noise
        // of power p gives each bin p after dividing by the window's power.
        let bin = |k: usize| {
            let m = (FRAME - k) % FRAME;
            let z = f64::from(self.re[k]).powi(2) + f64::from(self.im[k]).powi(2);
            let w = f64::from(self.re[m]).powi(2) + f64::from(self.im[m]).powi(2);
            (z + w) / 2.0 / (self.window_power * channels)
        };
        let bands = self
            .bins
            .map(|(low, high)| (low..=high).map(bin).sum::<f64>() / (high - low + 1) as f64);
        self.frames.push((power, bands));
    }

    /// The quietest frames' average spectrum as `afftdn` reads a profile.
    /// Sound with no noise at all gets the lowest floor `afftdn` takes.
    fn profile(mut self) -> NoiseProfile {
        self.frames.retain(|(power, _)| *power > SILENT_POWER);
        if self.frames.is_empty() {
            return NoiseProfile {
                floor: -80.0,
                bands: [0.0; NOISE_BANDS],
            };
        }
        let mut powers: Vec<f64> = self.frames.iter().map(|(power, _)| *power).collect();
        powers.sort_by(f64::total_cmp);
        let threshold = powers[((powers.len() - 1) as f64 * QUIET_SHARE) as usize];
        let quiet: Vec<&[f64; NOISE_BANDS]> = self
            .frames
            .iter()
            .filter(|(power, _)| *power <= threshold)
            .map(|(_, bands)| bands)
            .collect();
        let levels: [f64; NOISE_BANDS] = std::array::from_fn(|band| {
            let mean = quiet.iter().map(|bands| bands[band]).sum::<f64>() / quiet.len() as f64;
            10.0 * (mean + 1e-20).log10()
        });
        let mean = levels.iter().sum::<f64>() / NOISE_BANDS as f64;
        NoiseProfile {
            floor: mean.clamp(-80.0, -20.0) as f32,
            bands: levels.map(|level| (level - mean).clamp(-24.0, 24.0) as f32),
        }
    }
}

/// An in-place radix-2 complex FFT of one size.
struct Fft {
    cos: Vec<f32>,
    sin: Vec<f32>,
    reversed: Vec<usize>,
}

impl Fft {
    fn new(size: usize) -> Self {
        debug_assert!(size.is_power_of_two());
        let bits = size.trailing_zeros();
        let turn = |i: usize| -2.0 * std::f64::consts::PI * i as f64 / size as f64;
        Self {
            cos: (0..size / 2).map(|i| turn(i).cos() as f32).collect(),
            sin: (0..size / 2).map(|i| turn(i).sin() as f32).collect(),
            reversed: (0..size)
                .map(|i| i.reverse_bits() >> (usize::BITS - bits))
                .collect(),
        }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let size = re.len();
        for (i, &j) in self.reversed.iter().enumerate() {
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut half = 1;
        while half < size {
            let step = size / (half * 2);
            for start in (0..size).step_by(half * 2) {
                for k in 0..half {
                    let (c, s) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            half *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The centres `af_afftdn.c` computes at any rate.
    #[test]
    fn band_centres_match_afftdn() {
        assert_eq!(
            band_centres(),
            [
                80.0, 120.0, 180.0, 270.0, 410.0, 620.0, 930.0, 1400.0, 2100.0, 3150.0, 4750.0,
                7100.0, 10700.0, 16000.0, 24000.0
            ]
        );
    }

    /// One transform of a tone puts its power in its own bin and its mirror.
    #[test]
    fn the_transform_finds_a_tone_in_its_bin() {
        let fft = Fft::new(64);
        let mut re: Vec<f32> = (0..64)
            .map(|i| (2.0 * std::f32::consts::PI * 5.0 * i as f32 / 64.0).cos())
            .collect();
        let mut im = vec![0.0; 64];
        fft.run(&mut re, &mut im);
        for k in 0..64 {
            let power = re[k].hypot(im[k]);
            let want = if k == 5 || k == 59 { 32.0 } else { 0.0 };
            assert!((power - want).abs() < 1e-3, "bin {k}: {power}");
        }
    }
}

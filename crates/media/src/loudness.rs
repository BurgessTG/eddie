//! Streaming loudness: ITU-R BS.1770 / EBU R128 integrated loudness, true
//! peak, clipping and quiet ranges, measured while a mix is rendered.
//!
//! Ported from fframes' `audio_analysis.rs` (MIT, dmtrKovalenko/fframes at
//! 49b7606): libebur128's K-weighting formulas, BS.1770 gating (-70 LUFS
//! absolute, -10 LU relative, 400 ms blocks, 75% overlap) and the 4x
//! true-peak interpolator of Annex 2. Unlike fframes it streams: only one
//! energy and one peak per 100 ms hop are kept, never the samples.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::{AudioEngine, AudioSegment, AUDIO_RATE};
use crate::MediaResult;

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two K-weighting stages (libebur128 `ebur128_init_filter`).
fn k_weighting(rate: f64) -> [Biquad; 2] {
    let shelf = {
        let f0 = 1_681.974_450_955_533;
        let g = 3.999_843_853_973_347;
        let q = 0.707_175_236_955_419_6;
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let vh = 10f64.powf(g / 20.);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1. + k / q + k * k;
        Biquad {
            b: [
                (vh + vb * k / q + k * k) / a0,
                2. * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            a: [2. * (k * k - 1.) / a0, (1. - k / q + k * k) / a0],
            z: [0.; 2],
        }
    };
    let high_pass = {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let a0 = 1. + k / q + k * k;
        Biquad {
            b: [1., -2., 1.],
            a: [2. * (k * k - 1.) / a0, (1. - k / q + k * k) / a0],
            z: [0.; 2],
        }
    };
    [shelf, high_pass]
}

/// BS.1770 Annex 2: 4x oversampling, 4 phases of 12 taps.
const TRUE_PEAK_PHASES: [[f64; 12]; 4] = [
    [
        0.001_708_984_375,
        0.010_986_328_125,
        -0.019_653_320_312_5,
        0.033_203_125,
        -0.059_448_242_187_5,
        0.137_329_101_562_5,
        0.972_167_968_75,
        -0.102_294_921_875,
        0.047_607_421_875,
        -0.026_611_328_125,
        0.014_892_578_125,
        -0.008_300_781_25,
    ],
    [
        -0.029_174_804_687_5,
        0.029_296_875,
        -0.051_757_812_5,
        0.089_111_328_125,
        -0.166_503_906_25,
        0.465_087_890_625,
        0.779_785_156_25,
        -0.200_317_382_812_5,
        0.101_562_5,
        -0.058_227_539_062_5,
        0.033_081_054_687_5,
        -0.018_920_898_437_5,
    ],
    [
        -0.018_920_898_437_5,
        0.033_081_054_687_5,
        -0.058_227_539_062_5,
        0.101_562_5,
        -0.200_317_382_812_5,
        0.779_785_156_25,
        0.465_087_890_625,
        -0.166_503_906_25,
        0.089_111_328_125,
        -0.051_757_812_5,
        0.029_296_875,
        -0.029_174_804_687_5,
    ],
    [
        -0.008_300_781_25,
        0.014_892_578_125,
        -0.026_611_328_125,
        0.047_607_421_875,
        -0.102_294_921_875,
        0.972_167_968_75,
        0.137_329_101_562_5,
        -0.059_448_242_187_5,
        0.033_203_125,
        -0.019_653_320_312_5,
        0.010_986_328_125,
        0.001_708_984_375,
    ],
];

const ABSOLUTE_GATE: f64 = -70.0;

fn energy_to_lufs(mean_square: f64) -> f64 {
    if mean_square > 0. {
        -0.691 + 10. * mean_square.log10()
    } else {
        f64::NEG_INFINITY
    }
}

fn lufs_to_energy(lufs: f64) -> f64 {
    10f64.powf((lufs + 0.691) / 10.)
}

/// Feeds interleaved stereo and keeps one energy and one true peak per
/// 100 ms hop.
pub struct LoudnessMeter {
    rate: usize,
    hop: usize,
    filters: [[Biquad; 2]; 2],
    /// The last 12 samples per channel, newest first, for true peak.
    history: [[f64; 12]; 2],
    in_hop: usize,
    energy: f64,
    peak: f64,
    clipped: usize,
    frames: usize,
    hops: Vec<f64>,
    peaks: Vec<f64>,
}

impl LoudnessMeter {
    pub fn new(rate: usize) -> Self {
        let weighting = k_weighting(rate as f64);
        Self {
            rate,
            hop: (rate / 10).max(1),
            filters: [weighting, weighting],
            history: [[0.; 12]; 2],
            in_hop: 0,
            energy: 0.,
            peak: 0.,
            clipped: 0,
            frames: 0,
            hops: Vec::new(),
            peaks: Vec::new(),
        }
    }

    /// Interleaved stereo frames.
    pub fn push(&mut self, stereo: &[f32]) {
        for frame in stereo.chunks_exact(2) {
            for (channel, &sample) in frame.iter().enumerate() {
                let x = f64::from(sample);
                if sample.abs() > 1.0 {
                    self.clipped += 1;
                }
                let [shelf, high_pass] = &mut self.filters[channel];
                let y = high_pass.process(shelf.process(x));
                self.energy += y * y;
                let history = &mut self.history[channel];
                history.copy_within(0..11, 1);
                history[0] = x;
                let mut peak = x.abs();
                for phase in &TRUE_PEAK_PHASES {
                    let acc: f64 = phase.iter().zip(history.iter()).map(|(c, s)| c * s).sum();
                    peak = peak.max(acc.abs());
                }
                self.peak = self.peak.max(peak);
            }
            self.in_hop += 1;
            self.frames += 1;
            if self.in_hop == self.hop {
                self.hops.push(self.energy);
                self.peaks.push(self.peak);
                (self.in_hop, self.energy, self.peak) = (0, 0., 0.);
            }
        }
    }

    pub fn finish(mut self) -> Loudness {
        // Loudness counts whole hops only, as BS.1770 blocks do. A peak in
        // the unfinished last hop still counts: export would limit it.
        if self.in_hop > 0 {
            self.peaks.push(self.peak);
        }
        Loudness {
            hop_seconds: self.hop as f64 / self.rate as f64,
            hop: self.hop,
            seconds: self.frames as f64 / self.rate as f64,
            hops: self.hops,
            peaks: self.peaks,
            clipped: self.clipped,
        }
    }
}

/// A measured mix: loudness, peaks and quiet ranges over any time range.
#[derive(Clone, Debug, Default)]
pub struct Loudness {
    hop_seconds: f64,
    hop: usize,
    seconds: f64,
    hops: Vec<f64>,
    /// One per hop, plus one for an unfinished last hop.
    peaks: Vec<f64>,
    /// Samples beyond full scale, counted per channel.
    pub clipped: usize,
}

impl Loudness {
    /// The measured length in seconds.
    pub fn duration(&self) -> f64 {
        self.seconds
    }

    /// A time in hops. The margin keeps a time on a hop edge (2.9 s is
    /// 28.999... hops) from falling into the hop before it.
    fn hops_at(&self, seconds: f64) -> f64 {
        const MARGIN: f64 = 1e-9;
        if self.hop_seconds > 0.0 {
            // `max` drops a NaN; an infinite end saturates to the last hop.
            (seconds / self.hop_seconds + MARGIN).max(0.0)
        } else {
            0.0
        }
    }

    /// The whole hops inside `seconds`; empty when the range is reversed.
    fn hop_range(&self, seconds: Range<f64>) -> Range<usize> {
        let index = |t: f64| (self.hops_at(t).floor() as usize).min(self.hops.len());
        let start = index(seconds.start);
        start..index(seconds.end).max(start)
    }

    /// Every hop `seconds` touches, the unfinished last one included.
    fn peak_range(&self, seconds: Range<f64>) -> Range<usize> {
        const MARGIN: f64 = 2e-9;
        let start = (self.hops_at(seconds.start).floor() as usize).min(self.peaks.len());
        let end =
            ((self.hops_at(seconds.end) - MARGIN).ceil().max(0.0) as usize).min(self.peaks.len());
        start..end.max(start)
    }

    /// Mean squares of `width`-hop windows starting at every hop of `range`.
    fn windows(&self, range: Range<usize>, width: usize) -> impl Iterator<Item = f64> + '_ {
        let last = range
            .end
            .checked_sub(width)
            .filter(|&last| last >= range.start);
        let span = (width * self.hop) as f64;
        last.into_iter()
            .flat_map(move |last| range.start..=last)
            .map(move |i| self.hops[i..i + width].iter().sum::<f64>() / span)
    }

    /// Gated integrated loudness in LUFS; `None` for silence.
    pub fn integrated(&self, seconds: Range<f64>) -> Option<f64> {
        let absolute = lufs_to_energy(ABSOLUTE_GATE);
        let gated: Vec<f64> = self
            .windows(self.hop_range(seconds), 4)
            .filter(|z| *z >= absolute)
            .collect();
        if gated.is_empty() {
            return None;
        }
        let relative = gated.iter().sum::<f64>() / gated.len() as f64 * 0.1;
        let kept: Vec<f64> = gated.into_iter().filter(|z| *z >= relative).collect();
        let lufs = energy_to_lufs(kept.iter().sum::<f64>() / kept.len() as f64);
        lufs.is_finite().then_some(lufs)
    }

    /// Highest true peak in dBTP; `None` for digital silence.
    pub fn true_peak(&self, seconds: Range<f64>) -> Option<f64> {
        let peak = self.peaks[self.peak_range(seconds)]
            .iter()
            .fold(0.0_f64, |a, b| a.max(*b));
        (peak > 0.).then(|| 20. * peak.log10())
    }

    /// Loudest momentary (400 ms) loudness in LUFS.
    pub fn max_momentary(&self) -> Option<f64> {
        let max = self.windows(0..self.hops.len(), 4).fold(0.0, f64::max);
        (max > 0.).then(|| energy_to_lufs(max))
    }

    /// Ranges of at least `min_seconds` whose momentary loudness is below
    /// `threshold` LUFS.
    pub fn quiet_ranges(&self, threshold: f64, min_seconds: f64) -> Vec<Range<f64>> {
        let limit = lufs_to_energy(threshold);
        let momentary: Vec<f64> = self.windows(0..self.hops.len(), 4).collect();
        let mut ranges = Vec::new();
        let mut start = None;
        for (i, quiet) in momentary
            .iter()
            .map(|z| *z < limit)
            .chain([false])
            .enumerate()
        {
            match (start, quiet) {
                (None, true) => start = Some(i),
                (Some(s), false) => {
                    // A block covers four hops: the quiet part ends where
                    // the first block that hears sound ends.
                    let measured = self.hops.len() as f64 * self.hop_seconds;
                    let range = s as f64 * self.hop_seconds
                        ..((i + 3) as f64 * self.hop_seconds).min(measured);
                    if range.end - range.start >= min_seconds {
                        ranges.push(range);
                    }
                    start = None;
                }
                _ => {}
            }
        }
        ranges
    }
}

/// Render the mix of `segments` (as the preview mixer plays them, ducking
/// included) from 0 to `end` seconds offline and measure it. Decoders start
/// only near the render position, as in playback. `cancel` stops early.
pub fn measure_mix(
    segments: &[(u64, AudioSegment)],
    end: f64,
    cancel: &AtomicBool,
) -> MediaResult<Loudness> {
    const LOOK_AHEAD: f64 = 1.0;
    const BLOCK: usize = 4096;
    let mut engine = AudioEngine::manual();
    engine.seek(0.0);
    engine.set_limit(end);
    engine.set_running(true);
    let mut meter = LoudnessMeter::new(AUDIO_RATE as usize);
    let total = (end.max(0.0) * f64::from(AUDIO_RATE)).round() as usize;
    let mut rendered = 0;
    let mut live: Vec<(u64, AudioSegment)> = Vec::new();
    while rendered < total {
        if cancel.load(Ordering::Relaxed) {
            return Err("loudness measurement cancelled".to_owned());
        }
        let now = rendered as f64 / f64::from(AUDIO_RATE);
        live.clear();
        live.extend(
            segments
                .iter()
                .filter(|(_, s)| s.start < now + LOOK_AHEAD && s.start + s.duration > now)
                .cloned(),
        );
        engine.set_voices(&live);
        let block = engine.render(BLOCK.min(total - rendered));
        if block.is_empty() {
            return Err("loudness measurement stalled".to_owned());
        }
        rendered += block.len() / 2;
        meter.push(block);
        if let Some(error) = engine.take_error() {
            return Err(error);
        }
    }
    Ok(meter.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frequency: f64, amplitude: f32, seconds: f64, rate: usize) -> Vec<f32> {
        (0..(seconds * rate as f64) as usize)
            .map(|i| {
                amplitude
                    * (std::f64::consts::TAU * frequency * i as f64 / rate as f64).sin() as f32
            })
            .collect()
    }

    fn measure(left: &[f32], right: &[f32], rate: usize, chunk: usize) -> Loudness {
        let stereo: Vec<f32> = left.iter().zip(right).flat_map(|(l, r)| [*l, *r]).collect();
        let mut meter = LoudnessMeter::new(rate);
        for part in stereo.chunks(chunk * 2) {
            meter.push(part);
        }
        meter.finish()
    }

    #[test]
    fn full_scale_997hz_sine_in_one_channel_reads_minus_3_lufs() {
        // BS.1770: a 0 dBFS 997 Hz sine in one front channel is -3.01 LKFS.
        let left = sine(997., 1., 5., 48000);
        let right = vec![0.; left.len()];
        // An odd chunk size: hops must not depend on how samples arrive.
        let loudness = measure(&left, &right, 48000, 1237);
        let lufs = loudness.integrated(0.0..5.0).unwrap();
        assert!((lufs + 3.01).abs() < 0.05, "{lufs}");
        assert!(loudness.true_peak(0.0..5.0).unwrap().abs() < 0.1);
        assert_eq!(loudness.clipped, 0);
    }

    #[test]
    fn ebu_3341_stereo_minus_23() {
        let tone = sine(1000., 10f32.powf(-23. / 20.), 20., 48000);
        let lufs = measure(&tone, &tone, 48000, 4096)
            .integrated(0.0..20.0)
            .unwrap();
        assert!((lufs + 23.).abs() < 0.1, "{lufs}");
    }

    #[test]
    fn gating_ignores_silence_and_ranges_select_hops() {
        // 3 s of silence then 3 s of tone: the gate keeps the tone's loudness
        // over the whole file, and only the silence is quiet.
        let mut signal = vec![0.; 48000 * 3];
        signal.extend(sine(1000., 10f32.powf(-23. / 20.), 3., 48000));
        let loudness = measure(&signal, &signal, 48000, 4096);
        let whole = loudness.integrated(0.0..6.0).unwrap();
        let tone = loudness.integrated(3.0..6.0).unwrap();
        // Ungated, half silence would read 3 LU low; only the few blocks
        // straddling the edge pull it down.
        assert!((tone + 23.).abs() < 0.1, "{tone}");
        assert!(whole > tone - 0.3, "{whole} vs {tone}");
        assert_eq!(loudness.integrated(0.0..3.0), None);
        assert_eq!(loudness.true_peak(0.0..2.9), None);
        let quiet = loudness.quiet_ranges(-60., 1.);
        assert_eq!(quiet.len(), 1);
        assert!(quiet[0].start < 0.1 && (quiet[0].end - 3.0).abs() < 0.35);
    }

    #[test]
    fn a_peak_in_the_unfinished_last_hop_is_measured() {
        // 1.05 s: ten whole hops of quiet tone, then a click 20 ms from the
        // end, inside the half hop that loudness does not count.
        let mut signal = sine(1000., 0.1, 1.05, 48000);
        let click = signal.len() - 960;
        signal[click] = 0.9;
        let loudness = measure(&signal, &signal, 48000, 4096);
        assert!((loudness.duration() - 1.05).abs() < 1e-9);
        let whole = loudness.true_peak(0.0..loudness.duration()).unwrap();
        assert!(whole > -1.5, "the click is {whole} dBTP");
        // Before the click's hop only the tone is heard.
        let body = loudness.true_peak(0.0..1.0).unwrap();
        assert!((body + 20.).abs() < 0.5, "{body}");
        // The half hop adds no loudness block.
        let lufs = loudness.integrated(0.0..loudness.duration()).unwrap();
        assert_eq!(Some(lufs), loudness.integrated(0.0..1.0));
    }

    #[test]
    fn reversed_and_outside_ranges_are_empty() {
        let signal = sine(1000., 0.5, 2., 48000);
        let loudness = measure(&signal, &signal, 48000, 4096);
        assert_eq!(loudness.true_peak(1.5..0.5), None);
        assert_eq!(loudness.integrated(1.5..0.5), None);
        assert_eq!(loudness.true_peak(5.0..9.0), None);
        assert!(loudness.true_peak(f64::NAN..1.0).is_some());
        assert_eq!(
            loudness.integrated(-3.0..f64::INFINITY),
            loudness.integrated(0.0..2.0)
        );
        // A hop edge belongs to the hop that starts there: 1.9 s is
        // 18.999... hops in floating point.
        assert_eq!(loudness.hop_range(0.0..1.9), 0..19);
        assert_eq!(loudness.peak_range(0.0..1.9), 0..19);
        assert_eq!(loudness.peak_range(0.0..2.9), 0..20);
        assert_eq!(loudness.peak_range(0.3..0.31), 3..4);
        assert_eq!(Loudness::default().true_peak(0.0..1.0), None);
    }

    #[test]
    fn inter_sample_peaks_exceed_the_sample_peak() {
        // fs/4 at 45 degrees: every sample is 0.707 of the true crest.
        let rate = 48000;
        let signal: Vec<f32> = (0..rate)
            .map(|i| {
                (std::f64::consts::FRAC_PI_2 * i as f64 + std::f64::consts::FRAC_PI_4).sin() as f32
            })
            .collect();
        let loudness = measure(&signal, &signal, rate, 512);
        let peak = loudness.true_peak(0.0..1.0).unwrap();
        assert!(peak > -0.5, "true peak {peak} dBTP, sample peak -3 dBFS");
    }
}

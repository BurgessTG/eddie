//! Beat grid from the import waveform, for cutting to music.
//!
//! The 50/sec peak envelope is coarse but enough for tempo: onsets are
//! rises in log amplitude, a comb over every tempo from 70 to 180 BPM picks
//! the best-supported period and phase, and a least-squares fit through
//! beats snapped to nearby onsets removes drift over long tracks. This is a
//! steady-tempo tracker; rubato or tempo changes are not followed.

/// Detected tempo and beat times in source seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct BeatGrid {
    pub bpm: f64,
    pub beats: Vec<f64>,
}

const MIN_BPM: f64 = 70.0;
const MAX_BPM: f64 = 180.0;
/// Comb support must stand this far above the average onset strength,
/// and most grid beats must have an onset nearby. A 99 BPM funk track
/// measured 7.0 and 95%; arrhythmic bursts hit far fewer beats.
const MIN_CONTRAST: f64 = 3.0;
const MIN_HIT_SHARE: f64 = 0.75;

pub fn beat_grid(peaks: &[f32], peaks_per_sec: f64) -> Option<BeatGrid> {
    if !peaks_per_sec.is_finite() || peaks_per_sec <= 0.0 {
        return None;
    }
    // At least eight beats at the slowest tempo.
    if (peaks.len() as f64) < peaks_per_sec * 60.0 / MIN_BPM * 8.0 {
        return None;
    }
    let onset = onset_envelope(peaks, peaks_per_sec);
    let mean = onset.iter().sum::<f64>() / onset.len() as f64;
    if mean <= 0.0 {
        return None;
    }
    // (weighted score, raw score, period in buckets, phase in buckets)
    let mut best = (0.0, 0.0, 0.0, 0.0);
    let mut bpm = MIN_BPM;
    while bpm <= MAX_BPM + 1e-9 {
        let period = 60.0 * peaks_per_sec / bpm;
        let mut phase = 0.0;
        while phase < period {
            let score = comb(&onset, period, phase);
            // A weak prior toward 120 BPM settles octave ambiguity.
            let octaves = (bpm / 120.0).log2();
            let weighted = score * (-0.5 * octaves * octaves).exp();
            if weighted > best.0 {
                best = (weighted, score, period, phase);
            }
            phase += 0.5;
        }
        bpm += 0.1;
    }
    let (_, score, period, phase) = best;
    if score < mean * MIN_CONTRAST {
        return None;
    }
    // Snap each grid beat to the strongest onset within two buckets,
    // then fit one straight line through the snapped beats.
    let mut points = Vec::new();
    let mut index = 0.0;
    loop {
        let at = phase + index * period;
        if at >= onset.len() as f64 {
            break;
        }
        let center = at.round() as isize;
        let snapped = (center - 2..=center + 2)
            .filter(|i| *i >= 0 && (*i as usize) < onset.len())
            .max_by(|a, b| onset[*a as usize].total_cmp(&onset[*b as usize]))
            .filter(|i| onset[*i as usize] > mean)
            .map(|i| i as f64);
        if let Some(snapped) = snapped {
            points.push((index, snapped));
        }
        index += 1.0;
    }
    if (points.len() as f64) < index * MIN_HIT_SHARE {
        return None;
    }
    let (period, phase) = fit_line(&points).unwrap_or((period, phase));
    let mut beats = Vec::new();
    let mut k = 0.0;
    let end = peaks.len() as f64 / peaks_per_sec;
    loop {
        // Bucket i covers [i, i+1); the onset is at its start.
        let time = (phase + k * period) / peaks_per_sec;
        if time >= end {
            break;
        }
        if time >= 0.0 {
            beats.push(time);
        }
        k += 1.0;
    }
    Some(BeatGrid {
        bpm: 60.0 * peaks_per_sec / period,
        beats,
    })
}

/// Positive rises in log amplitude, less their local half-second mean.
fn onset_envelope(peaks: &[f32], peaks_per_sec: f64) -> Vec<f64> {
    let level: Vec<f64> = peaks
        .iter()
        .map(|p| (1.0 + 100.0 * f64::from(p.abs().min(1.0))).ln())
        .collect();
    let rise: Vec<f64> = level
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                0.0
            } else {
                (l - level[i - 1]).max(0.0)
            }
        })
        .collect();
    let half = ((peaks_per_sec * 0.25).round() as usize).max(1);
    let mut prefix = vec![0.0; rise.len() + 1];
    for (i, r) in rise.iter().enumerate() {
        prefix[i + 1] = prefix[i] + r;
    }
    (0..rise.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(rise.len());
            let local = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            (rise[i] - local).max(0.0)
        })
        .collect()
}

fn comb(onset: &[f64], period: f64, phase: f64) -> f64 {
    let mut sum = 0.0;
    let mut count = 0;
    let mut at = phase;
    while at < onset.len() as f64 {
        let i = at.round() as usize;
        if i < onset.len() {
            sum += onset[i];
            count += 1;
        }
        at += period;
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// Least-squares `y = phase + k * period`, or None for too few points.
fn fit_line(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    if points.len() < 4 {
        return None;
    }
    let n = points.len() as f64;
    let mean_k = points.iter().map(|p| p.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
    let covariance: f64 = points.iter().map(|p| (p.0 - mean_k) * (p.1 - mean_y)).sum();
    let variance: f64 = points.iter().map(|p| (p.0 - mean_k).powi(2)).sum();
    if variance <= 0.0 {
        return None;
    }
    let period = covariance / variance;
    (period > 0.0).then(|| (period, mean_y - period * mean_k))
}

#[cfg(test)]
mod tests {
    use super::beat_grid;

    const RATE: f64 = 50.0;

    /// Kick-like peaks on every beat over a quiet bed, with louder
    /// off-beat hats so half-tempo and double-tempo combs both score.
    fn track(bpm: f64, offset: f64, seconds: f64) -> Vec<f32> {
        let mut peaks = vec![0.05_f32; (seconds * RATE) as usize];
        let period = 60.0 / bpm;
        let mut t = offset;
        while t < seconds {
            let i = (t * RATE).floor() as usize;
            for (j, level) in [(0, 0.9_f32), (1, 0.6), (2, 0.3)] {
                if let Some(p) = peaks.get_mut(i + j) {
                    *p = level;
                }
            }
            let hat = ((t + period / 2.0) * RATE).floor() as usize;
            if let Some(p) = peaks.get_mut(hat) {
                *p = p.max(0.25);
            }
            t += period;
        }
        // Deterministic jitter in the bed so the envelope is not flat.
        for (i, p) in peaks.iter_mut().enumerate() {
            *p += ((i * 7919 % 13) as f32) * 0.002;
        }
        peaks
    }

    #[test]
    fn finds_tempo_and_phase_without_drift_over_a_long_track() {
        for (bpm, offset) in [(99.0, 0.31), (128.0, 0.07), (91.5, 0.5)] {
            let grid = beat_grid(&track(bpm, offset, 150.0), RATE).expect("rhythmic track");
            assert!((grid.bpm - bpm).abs() < 0.3, "{bpm}: got {}", grid.bpm);
            let period = 60.0 / bpm;
            // Every detected beat lands within one bucket of a true beat,
            // including the last ones, where a tempo error accumulates.
            for beat in &grid.beats {
                let k = ((beat - offset) / period).round();
                let truth = offset + k * period;
                assert!(
                    (beat - truth).abs() <= 0.021,
                    "{bpm}: beat {beat} vs {truth}"
                );
            }
            let expected = ((150.0 - offset) / period).ceil() as usize;
            assert!(
                grid.beats.len().abs_diff(expected) <= 1,
                "{bpm}: {}",
                grid.beats.len()
            );
        }
    }

    #[test]
    fn arrhythmic_audio_has_no_grid() {
        // Speech-like bursts at irregular spacing.
        let mut peaks = vec![0.02_f32; 60 * RATE as usize];
        let mut i = 0;
        let mut step = 17;
        while i < peaks.len() {
            for p in peaks.iter_mut().skip(i).take(step % 9 + 4) {
                *p = 0.5;
            }
            step = (step * 31 + 11) % 97 + 13;
            i += step;
        }
        assert_eq!(beat_grid(&peaks, RATE), None);
        assert_eq!(beat_grid(&[0.0; 3000], RATE), None);
        assert_eq!(beat_grid(&[0.5; 100], RATE), None);
    }
}

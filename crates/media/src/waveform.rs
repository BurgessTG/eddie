//! Waveform peaks, and the silence and sound read from them.

use std::io::Read;
use std::process::{Command, Stdio};

use crate::MediaResult;

fn accumulate(
    sample: i16,
    per_peak: usize,
    peaks: &mut Vec<f32>,
    current: &mut f32,
    count: &mut usize,
) {
    *current = current.max(sample.unsigned_abs() as f32 / 32768.0);
    *count += 1;
    if *count >= per_peak {
        peaks.push(*current);
        *current = 0.0;
        *count = 0;
    }
}

/// Import waveform resolution. Fifty buckets per second is fine
/// enough for phrase-level silence cuts without retaining audio.
pub const WAVEFORM_PEAKS_PER_SEC: u32 = 50;

/// Peak waveform: one max-amplitude value per `1/peaks_per_sec` seconds.
pub fn waveform_peaks(path: &str, duration: f64, peaks_per_sec: u32) -> MediaResult<Vec<f32>> {
    let sample_rate = 8000_u32;
    // Stream PCM in fixed chunks: peak memory stays constant regardless of
    // file length (the old version buffered the entire decode).
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            path,
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg stdout unavailable".to_owned())?;
    let per_peak = (sample_rate / peaks_per_sec.max(1)).max(1) as usize;
    // The duration comes from the file, so it is a hint and never a size
    // to trust: a container claiming years would reserve gigabytes.
    let expected = (duration * f64::from(peaks_per_sec)).clamp(0.0, (1 << 22) as f64);
    let mut peaks = Vec::with_capacity(expected as usize + 1);
    let mut current = 0.0_f32;
    let mut count = 0_usize;
    let mut buffer = [0_u8; 64 * 1024];
    let mut carry: Option<u8> = None;
    loop {
        let read = stdout
            .read(&mut buffer)
            .map_err(|e| format!("read ffmpeg stdout: {e}"))?;
        if read == 0 {
            break;
        }
        let mut bytes = &buffer[..read];
        if let Some(first) = carry.take() {
            if !bytes.is_empty() {
                accumulate(
                    i16::from_le_bytes([first, bytes[0]]),
                    per_peak,
                    &mut peaks,
                    &mut current,
                    &mut count,
                );
                bytes = &bytes[1..];
            }
        }
        let (pairs, rest) = bytes.as_chunks::<2>();
        for pair in pairs {
            accumulate(
                i16::from_le_bytes(*pair),
                per_peak,
                &mut peaks,
                &mut current,
                &mut count,
            );
        }
        carry = rest.first().copied();
    }
    if count > 0 {
        peaks.push(current);
    }
    let status = child.wait().map_err(|e| format!("ffmpeg wait: {e}"))?;
    if peaks.is_empty() && !status.success() {
        return Err("waveform decode failed".to_owned());
    }
    Ok(peaks)
}

/// A half-open source-time range, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

fn valid_peak_rate(peaks_per_sec: f64) -> bool {
    peaks_per_sec.is_finite() && peaks_per_sec > 0.0
}

/// Peak level at or under which speech recorded near full scale is silent.
pub const SILENCE_PEAK: f32 = 0.03;

/// The peak level at or under which this recording counts as silent. It
/// follows the recording: 4% of its loud level (the 99th-percentile peak),
/// which is `SILENCE_PEAK` near full scale and never more. A fixed level
/// called a quiet take's soft syllables silence. It is never under 0.002
/// either, so a file without sound stays silent.
pub fn silence_threshold(peaks: &[f32]) -> f32 {
    const SHARE_OF_LOUD: f32 = 0.04;
    const FLOOR: f32 = 0.002;
    let mut levels: Vec<f32> = peaks
        .iter()
        .copied()
        .filter(|peak| peak.is_finite())
        .collect();
    if levels.is_empty() {
        return SILENCE_PEAK;
    }
    let index = (levels.len() - 1) * 99 / 100;
    let (_, loud, _) = levels.select_nth_unstable_by(index, f32::total_cmp);
    (*loud * SHARE_OF_LOUD).clamp(FLOOR, SILENCE_PEAK)
}

/// Runs of waveform buckets at or below `threshold`, keeping only
/// runs at least `min_duration` long. This is deliberately DSP,
/// not speech recognition: a quiet room tone and true silence are
/// separated by the caller's threshold, while meaning/filler words
/// still come from transcription.
pub fn silence_ranges(
    peaks: &[f32],
    peaks_per_sec: f64,
    threshold: f32,
    min_duration: f64,
) -> Vec<TimeRange> {
    if peaks.is_empty() || !valid_peak_rate(peaks_per_sec) {
        return Vec::new();
    }
    let threshold = threshold.clamp(0.0, 1.0);
    let min_duration = min_duration.max(0.0);
    let mut ranges = Vec::new();
    let mut run_start: Option<usize> = None;
    for (index, peak) in peaks.iter().copied().enumerate() {
        let silent = peak.is_finite() && peak <= threshold;
        match (silent, run_start) {
            (true, None) => run_start = Some(index),
            (false, Some(start)) => {
                let range = TimeRange {
                    start: start as f64 / peaks_per_sec,
                    end: index as f64 / peaks_per_sec,
                };
                if range.duration() + f64::EPSILON >= min_duration {
                    ranges.push(range);
                }
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = run_start {
        let range = TimeRange {
            start: start as f64 / peaks_per_sec,
            end: peaks.len() as f64 / peaks_per_sec,
        };
        if range.duration() + f64::EPSILON >= min_duration {
            ranges.push(range);
        }
    }
    ranges
}

/// Audible source ranges inside `[range_start, range_end)`.
/// Silences shorter than `min_silence` remain inside a phrase;
/// each kept phrase is expanded by `padding` on both sides so
/// tightening does not clip breaths or consonant attacks.
#[allow(clippy::too_many_arguments)]
pub fn audible_ranges(
    peaks: &[f32],
    peaks_per_sec: f64,
    range_start: f64,
    range_end: f64,
    threshold: f32,
    min_silence: f64,
    padding: f64,
) -> Vec<TimeRange> {
    if peaks.is_empty() || !valid_peak_rate(peaks_per_sec) {
        return Vec::new();
    }
    let media_end = peaks.len() as f64 / peaks_per_sec;
    let range_start = range_start.clamp(0.0, media_end);
    let range_end = range_end.clamp(range_start, media_end);
    if range_end <= range_start {
        return Vec::new();
    }
    let threshold = threshold.clamp(0.0, 1.0);
    let min_silence = min_silence.max(0.0);
    let padding = padding.max(0.0);
    let start_index = (range_start * peaks_per_sec).floor() as usize;
    let end_index = ((range_end * peaks_per_sec).ceil() as usize).min(peaks.len());

    let mut groups: Vec<TimeRange> = Vec::new();
    let mut group_start: Option<usize> = None;
    let mut previous_audible: Option<usize> = None;
    for (index, &peak) in peaks.iter().enumerate().take(end_index).skip(start_index) {
        if !peak.is_finite() || peak <= threshold {
            continue;
        }
        match (group_start, previous_audible) {
            (None, _) => group_start = Some(index),
            (Some(start), Some(previous)) => {
                let silent_buckets = index.saturating_sub(previous + 1);
                let silence = silent_buckets as f64 / peaks_per_sec;
                if silent_buckets > 0 && silence + f64::EPSILON >= min_silence {
                    groups.push(TimeRange {
                        start: start as f64 / peaks_per_sec,
                        end: (previous + 1) as f64 / peaks_per_sec,
                    });
                    group_start = Some(index);
                }
            }
            _ => {}
        }
        previous_audible = Some(index);
    }
    if let (Some(start), Some(previous)) = (group_start, previous_audible) {
        groups.push(TimeRange {
            start: start as f64 / peaks_per_sec,
            end: (previous + 1) as f64 / peaks_per_sec,
        });
    }

    let mut merged: Vec<TimeRange> = Vec::new();
    for group in groups {
        let range = TimeRange {
            start: (group.start - padding).clamp(range_start, range_end),
            end: (group.end + padding).clamp(range_start, range_end),
        };
        if range.duration() <= f64::EPSILON {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_peak_handles_full_scale_negative_sample() {
        let mut peaks = Vec::new();
        let mut current = 0.0;
        let mut count = 0;
        accumulate(i16::MIN, 1, &mut peaks, &mut current, &mut count);
        assert_eq!(peaks, vec![1.0]);
    }

    #[test]
    fn waveform_silence_and_audible_ranges_are_source_timed() {
        let mut peaks = vec![0.0; 5];
        peaks.extend([0.8; 5]);
        peaks.extend([0.0; 5]);
        peaks.extend([0.7; 5]);

        let silences = silence_ranges(&peaks, 10.0, 0.03, 0.35);
        assert_eq!(
            silences,
            vec![
                TimeRange {
                    start: 0.0,
                    end: 0.5,
                },
                TimeRange {
                    start: 1.0,
                    end: 1.5,
                },
            ]
        );

        let audible = audible_ranges(&peaks, 10.0, 0.0, 2.0, 0.03, 0.35, 0.1);
        assert_eq!(
            audible,
            vec![
                TimeRange {
                    start: 0.4,
                    end: 1.1,
                },
                TimeRange {
                    start: 1.4,
                    end: 2.0,
                },
            ]
        );
    }

    #[test]
    fn a_quiet_recording_has_the_same_pauses_as_a_loud_one() {
        // A take peaking near 0.08 had its soft trailing syllables under the
        // fixed 0.03: 18 s of its 45 s read as silence, and about 5 s was.
        let mut loud = Vec::new();
        for _ in 0..4 {
            loud.extend([0.6_f32; 40]);
            loud.extend([0.08; 20]); // a soft trailing syllable
            loud.extend([0.004; 30]); // the pause, with room tone
        }
        let quiet: Vec<f32> = loud.iter().map(|peak| peak * 0.1).collect();
        let pauses = |peaks: &[f32]| silence_ranges(peaks, 50.0, silence_threshold(peaks), 0.35);
        assert_eq!(pauses(&loud).len(), 4);
        assert_eq!(pauses(&quiet), pauses(&loud));
        assert!((pauses(&loud)[0].duration() - 0.6).abs() < 1e-9);
        // Speech near full scale keeps the level it always had, and a file
        // without sound is silent throughout.
        assert_eq!(silence_threshold(&[0.9; 100]), SILENCE_PEAK);
        assert_eq!(pauses(&[0.0004; 200]).len(), 1);
        assert_eq!(silence_threshold(&[]), SILENCE_PEAK);
    }

    #[test]
    fn short_waveform_pauses_stay_inside_one_audible_range() {
        let peaks = [0.8, 0.8, 0.0, 0.0, 0.7, 0.7];
        let audible = audible_ranges(&peaks, 10.0, 0.0, 0.6, 0.03, 0.35, 0.0);
        assert_eq!(
            audible,
            vec![TimeRange {
                start: 0.0,
                end: 0.6,
            }]
        );
    }
}

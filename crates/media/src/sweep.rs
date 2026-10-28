//! Lightweight QC analysis for imported footage.
//!
//! The sweep samples decoded frames at a small size, scores frame-to-frame
//! motion for stability and scene changes, then joins those samples with the
//! already-computed audio peaks. It returns edit-sized segments only; ranking
//! and user-facing reasons live in the app layer.

use std::path::Path;
use std::process::Command;

use crate::decode::VideoDecoder;
use crate::{run, MediaInfo, MediaResult};

#[derive(Clone, Debug, PartialEq)]
pub struct MotionSample {
    pub time: f64,
    /// Mean absolute RGB difference from the previous sampled frame, 0..=1.
    pub difference: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SweepSegment {
    pub start: f64,
    pub end: f64,
    pub mean_motion: f32,
    pub peak_motion: f32,
    /// 1 is locked-off/steady, 0 is very shaky or busy.
    pub stability: f32,
    pub audio_energy: f32,
    pub audio_peak: f32,
    /// Share of audio buckets at audible level, 0..1. Speech
    /// pauses between phrases; a mixed music bed stays near 1.
    pub audio_coverage: f32,
    pub scene_change: f32,
}

impl SweepSegment {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// Sample frame-to-frame motion at roughly 5 fps with the in-process decoder.
/// Frames are decoded at 64 px wide and buffers are recycled.
pub fn motion_samples(path: &str, info: &MediaInfo) -> MediaResult<Vec<MotionSample>> {
    if !info.has_video || info.duration <= 0.0 {
        return Ok(Vec::new());
    }
    let mut decoder = VideoDecoder::open(path, 64, true)?;
    let mut samples = Vec::new();
    let mut spare: Vec<u8> = Vec::new();
    let mut previous: Option<Vec<u8>> = None;
    let mut previous_time: Option<f64> = None;

    while let Some((time, frame)) = decoder.next_frame(spare) {
        spare = Vec::new();
        let should_sample = previous_time
            .map(|previous| time - previous >= 0.19)
            .unwrap_or(true);
        if !should_sample {
            spare = frame.rgba;
            continue;
        }
        if let Some(previous_frame) = previous.as_ref() {
            samples.push(MotionSample {
                time,
                difference: frame_difference(previous_frame, &frame.rgba),
            });
        } else {
            samples.push(MotionSample {
                time,
                difference: 0.0,
            });
        }
        previous_time = Some(time);
        if let Some(old) = previous.replace(frame.rgba) {
            spare = old;
        }
    }
    Ok(samples)
}

fn frame_difference(previous: &[u8], current: &[u8]) -> f32 {
    let len = previous.len().min(current.len());
    if len < 4 {
        return 0.0;
    }
    // Sample every 16th pixel. That is enough for shake/scene detection and
    // keeps a long recording's sweep proportional to decode, not analysis.
    let mut total = 0_u64;
    let mut count = 0_u64;
    let mut index = 0_usize;
    while index + 2 < len {
        total += (previous[index] as i32 - current[index] as i32).unsigned_abs() as u64;
        total += (previous[index + 1] as i32 - current[index + 1] as i32).unsigned_abs() as u64;
        total += (previous[index + 2] as i32 - current[index + 2] as i32).unsigned_abs() as u64;
        count += 3;
        index += 16 * 4;
    }
    if count == 0 {
        0.0
    } else {
        (total as f32 / count as f32 / 255.0).clamp(0.0, 1.0)
    }
}

/// Build 2.5–8 second candidate segments, preferring scene changes as cuts.
pub fn build_segments(
    duration: f64,
    motion: &[MotionSample],
    peaks: &[f32],
    peaks_per_sec: u32,
) -> Vec<SweepSegment> {
    if duration <= 0.0 {
        return Vec::new();
    }
    const MIN_SEGMENT: f64 = 2.5;
    const MAX_SEGMENT: f64 = 8.0;
    const SCENE_THRESHOLD: f32 = 0.18;

    let mut segments = Vec::new();
    let mut start = 0.0_f64;
    while start < duration {
        let hard_end = (start + MAX_SEGMENT).min(duration);
        let scene_end = motion
            .iter()
            .filter(|sample| {
                sample.time >= start + MIN_SEGMENT
                    && sample.time <= hard_end
                    && sample.difference >= SCENE_THRESHOLD
            })
            .map(|sample| sample.time)
            .next();
        let remaining = duration - start;
        let mut end = scene_end.unwrap_or_else(|| {
            if remaining > MAX_SEGMENT && remaining < MAX_SEGMENT + MIN_SEGMENT {
                start + remaining / 2.0
            } else {
                hard_end
            }
        });
        if duration - end > 0.0
            && duration - end < MIN_SEGMENT
            && duration - MIN_SEGMENT >= start + MIN_SEGMENT
        {
            end = duration - MIN_SEGMENT;
        }
        if end <= start {
            break;
        }
        segments.push(segment_metrics(start, end, motion, peaks, peaks_per_sec));
        start = end;
    }
    segments
}

fn segment_metrics(
    start: f64,
    end: f64,
    motion: &[MotionSample],
    peaks: &[f32],
    peaks_per_sec: u32,
) -> SweepSegment {
    let mut motion_sum = 0.0_f32;
    let mut motion_count = 0_usize;
    let mut peak_motion = 0.0_f32;
    let mut scene_change = 0.0_f32;
    for sample in motion {
        if sample.time > start && sample.time < end {
            motion_sum += sample.difference;
            motion_count += 1;
            peak_motion = peak_motion.max(sample.difference);
        }
        if (sample.time - start).abs() <= 0.25 {
            scene_change = scene_change.max(sample.difference);
        }
    }
    let mean_motion = if motion_count == 0 {
        0.0
    } else {
        motion_sum / motion_count as f32
    };
    // Handheld shake and busy test patterns quickly exceed ~0.10 mean sampled
    // RGB difference; locked shots sit near zero.
    let stability = (1.0 - mean_motion / 0.10).clamp(0.0, 1.0);

    let pps = peaks_per_sec.max(1) as f64;
    let first = (start * pps).floor().max(0.0) as usize;
    let last = ((end * pps).ceil() as usize).min(peaks.len());
    let (mut energy_sum, mut audio_peak, mut audible) = (0.0_f32, 0.0_f32, 0_usize);
    if first < last {
        for peak in &peaks[first..last] {
            energy_sum += peak * peak;
            audio_peak = audio_peak.max(*peak);
            if *peak >= 0.12 {
                audible += 1;
            }
        }
    }
    let audio_energy = if first < last {
        (energy_sum / (last - first) as f32).sqrt()
    } else {
        0.0
    };
    let audio_coverage = if first < last {
        audible as f32 / (last - first) as f32
    } else {
        0.0
    };

    SweepSegment {
        start,
        end,
        mean_motion,
        peak_motion,
        stability,
        audio_energy,
        audio_peak,
        audio_coverage,
        scene_change,
    }
}

/// Extract a mono 24 kHz PCM WAV chunk in the shape Meta's batch ASR accepts.
/// Metadata is stripped so caller context does not ride along in the upload.
pub fn write_wav_24k(path: &str, start: f64, duration: f64, output: &Path) -> MediaResult<()> {
    run(Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-ss",
            &format!("{:.3}", start.max(0.0)),
            "-t",
            &format!("{:.3}", duration.max(0.0)),
            "-i",
            path,
            "-vn",
            "-ac",
            "1",
            "-ar",
            "24000",
            "-c:a",
            "pcm_s16le",
            "-map_metadata",
            "-1",
            "-f",
            "wav",
        ])
        .arg(output))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_difference_detects_changed_pixels() {
        let previous = vec![0_u8; 64 * 4];
        let mut current = previous.clone();
        current[0] = 255;
        current[1] = 255;
        current[2] = 255;
        assert!(frame_difference(&previous, &current) > 0.0);
        assert_eq!(frame_difference(&previous, &previous), 0.0);
    }

    #[test]
    fn segmentation_prefers_scene_spike_and_caps_length() {
        let motion: Vec<MotionSample> = (0..=60)
            .map(|i| MotionSample {
                time: i as f64 * 0.5,
                difference: if i == 8 { 0.5 } else { 0.01 },
            })
            .collect();
        let peaks = vec![0.5; 600];
        let segments = build_segments(30.0, &motion, &peaks, 20);
        assert_eq!(segments[0].end, 4.0);
        assert!(segments.iter().all(|s| s.duration() <= 8.01));
        assert!(segments[0].stability > 0.5);
        assert!(segments[0].audio_peak > 0.4);
    }

    #[test]
    fn audio_coverage_separates_beds_from_pauses() {
        // Constant loud peaks: every bucket audible (music bed shape).
        let bed = build_segments(4.0, &[], &[0.6; 80], 20);
        assert!(bed[0].audio_coverage > 0.95, "{}", bed[0].audio_coverage);
        // Sparse peaks: mostly silence between hits (speech shape).
        let mut sparse = vec![0.0_f32; 80];
        sparse[10] = 0.9;
        sparse[40] = 0.8;
        let speech = build_segments(4.0, &[], &sparse, 20);
        assert!(
            speech[0].audio_coverage < 0.1,
            "{}",
            speech[0].audio_coverage
        );
    }

    #[test]
    fn segmentation_handles_audio_only() {
        let segments = build_segments(9.0, &[], &[0.25; 180], 20);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[1].end, 9.0);
    }
}

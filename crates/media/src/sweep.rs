//! Lightweight QC analysis for imported footage.
//!
//! The sweep samples decoded frames at a small size, scores frame-to-frame
//! motion for stability and scene changes, then joins those samples with the
//! already-computed audio peaks. It returns edit-sized segments only; ranking
//! and user-facing reasons live in the app layer.

use crate::decode::VideoDecoder;
use crate::{run, MediaInfo, MediaResult};
use continuity::{OcclusionExcursion, SceneContinuity};
use scene::{
    block_histogram_difference, detect_scene_cuts_grouped, frame_difference, histogram_difference,
    rgb_histograms, scene_score, Histograms, MIN_SCENE_SCORE,
};
use visual::aggregate_visual_evidence;

mod continuity;
mod excerpt;
mod scene;
mod visual;

pub use excerpt::{pcm_mono_16k, write_wav_24k};
pub use scene::detect_scene_cuts;
pub use visual::{frame_visual_evidence, FrameVisualEvidence, SegmentVisualEvidence};

#[derive(Clone, Debug, PartialEq)]
pub struct MotionSample {
    pub time: f64,
    /// Mean absolute RGB difference from the previous frame, 0..=1.
    /// This drives motion and stability metrics.
    pub difference: f32,
    /// Visual scene-change evidence for this frame, 0..=1. It is
    /// deliberately separate from motion so camera movement alone
    /// does not automatically become a cut.
    pub scene_score: f32,
    pub visual: FrameVisualEvidence,
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
    pub visual: Option<SegmentVisualEvidence>,
}

impl SweepSegment {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// Analyze every decoded frame at 64 px wide. Scene cuts can be a
/// single frame apart in edited footage, so sampling at a lower
/// cadence would make frame-accurate boundaries impossible.
/// Extract one representative JPEG frame for an image-model
/// request. This buffers one still only, never a video decode.
pub fn frame_jpeg_at(path: &str, time: f64, max_width: u32) -> MediaResult<Vec<u8>> {
    let max_width = max_width.clamp(64, 1920);
    let output = run(crate::cli::ffmpeg().args([
        "-v",
        "error",
        "-ss",
        &format!("{:.3}", time.max(0.0)),
        "-i",
        path,
        "-frames:v",
        "1",
        "-vf",
        &format!("scale=min({max_width}\\,iw):-2:flags=fast_bilinear"),
        "-q:v",
        "5",
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "-",
    ]))?;
    if output.stdout.len() < 4
        || output.stdout[..2] != [0xff, 0xd8]
        || output.stdout[output.stdout.len() - 2..] != [0xff, 0xd9]
    {
        return Err("ffmpeg returned no JPEG frame".to_owned());
    }
    Ok(output.stdout)
}

pub fn motion_samples(path: &str, info: &MediaInfo) -> MediaResult<Vec<MotionSample>> {
    if !info.has_video || info.duration <= 0.0 {
        return Ok(Vec::new());
    }
    let mut decoder = VideoDecoder::open(path, 64, true)?;
    let mut samples = Vec::new();
    let mut spare: Vec<u8> = Vec::new();
    let mut previous: Option<Vec<u8>> = None;
    let mut previous_histograms: Option<Histograms> = None;
    let mut continuity = SceneContinuity::default();
    let mut occlusion = OcclusionExcursion::default();

    while let Some((time, frame)) = decoder.try_next_frame(spare)? {
        spare = Vec::new();
        let histograms = rgb_histograms(&frame.rgba, frame.width, frame.height);
        let visual =
            frame_visual_evidence(previous.as_deref(), &frame.rgba, frame.width, frame.height);
        if let (Some(previous_frame), Some(previous_histograms)) = (&previous, &previous_histograms)
        {
            let difference = frame_difference(previous_frame, &frame.rgba);
            let histogram_difference = histogram_difference(&previous_histograms.0, &histograms.0);
            let block_difference =
                block_histogram_difference(&previous_histograms.1, &histograms.1);
            let mut score = scene_score(difference, histogram_difference, block_difference);
            if score >= MIN_SCENE_SCORE {
                score *= continuity.score_weight(
                    previous_frame,
                    &frame.rgba,
                    frame.width as usize,
                    frame.height as usize,
                );
            }
            samples.push(MotionSample {
                time,
                difference,
                scene_score: score,
                visual,
            });
        } else {
            samples.push(MotionSample {
                time,
                difference: 0.0,
                scene_score: 0.0,
                visual,
            });
        }
        occlusion.observe(&mut samples, &frame, &histograms, &mut continuity);
        previous_histograms = Some(histograms);
        if let Some(old) = previous.replace(frame.rgba) {
            spare = old;
        }
    }
    Ok(samples)
}

/// Build candidate segments from visual shots. Every detected
/// scene cut is a boundary; audio never creates one. Only shots
/// longer than the candidate cap are subdivided, preferring a
/// low-motion frame inside the shot for that artificial split.
pub fn build_segments(
    duration: f64,
    motion: &[MotionSample],
    peaks: &[f32],
    peaks_per_sec: u32,
) -> Vec<SweepSegment> {
    build_segments_inner(duration, motion, peaks, peaks_per_sec, 0.2, Some(8.0), 0.25)
}

/// Build a complete, source-ordered scene breakdown. Unlike
/// curator candidates, long shots are not subdivided at an
/// editorial cap, and adjacent detected boundaries are not
/// merged. Even a one-frame VFR insert keeps its exact PTS.
/// The returned spans cover `[0, duration]` contiguously.
pub fn build_scene_segments(
    duration: f64,
    motion: &[MotionSample],
    peaks: &[f32],
    peaks_per_sec: u32,
) -> Vec<SweepSegment> {
    build_segments_inner(duration, motion, peaks, peaks_per_sec, 0.0, None, 0.0)
}

fn build_segments_inner(
    duration: f64,
    motion: &[MotionSample],
    peaks: &[f32],
    peaks_per_sec: u32,
    min_shot: f64,
    max_segment: Option<f64>,
    group_window: f64,
) -> Vec<SweepSegment> {
    if duration <= 0.0 {
        return Vec::new();
    }
    const MIN_REMAINDER: f64 = 2.0;

    let mut boundaries = vec![0.0_f64];
    for cut in detect_scene_cuts_grouped(motion, group_window) {
        if cut - boundaries.last().copied().unwrap_or(0.0) >= min_shot && duration - cut >= min_shot
        {
            boundaries.push(cut);
        }
    }
    boundaries.push(duration);

    let mut segments = Vec::new();
    for span in boundaries.windows(2) {
        let shot_start = span[0];
        let shot_end = span[1];
        let mut start = shot_start;
        while max_segment
            .map(|max_segment| shot_end - start > max_segment)
            .unwrap_or(false)
        {
            let max_segment = max_segment.expect("checked by loop condition");
            let remaining = shot_end - start;
            let split = if remaining < max_segment + MIN_REMAINDER {
                start + remaining / 2.0
            } else {
                let latest = (start + max_segment).min(shot_end - MIN_REMAINDER);
                motion
                    .iter()
                    .filter(|sample| sample.time >= start + 4.0 && sample.time <= latest)
                    .min_by(|a, b| a.difference.total_cmp(&b.difference))
                    .map(|sample| sample.time)
                    .unwrap_or(start + max_segment)
            };
            if split <= start || split >= shot_end {
                break;
            }
            segments.push(analyze_segment(start, split, motion, peaks, peaks_per_sec));
            start = split;
        }
        if shot_end > start {
            segments.push(analyze_segment(
                start,
                shot_end,
                motion,
                peaks,
                peaks_per_sec,
            ));
        }
    }
    segments
}

/// Measure a source subrange using its own motion, visual, and audio samples.
pub fn analyze_segment(
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
    let mut visual_samples: Vec<&MotionSample> = Vec::new();
    for sample in motion {
        if sample.time >= start && sample.time < end {
            visual_samples.push(sample);
        }
        if sample.time > start && sample.time < end {
            motion_sum += sample.difference;
            motion_count += 1;
            peak_motion = peak_motion.max(sample.difference);
        }
        if (sample.time - start).abs() <= 0.25 {
            scene_change = scene_change.max(sample.scene_score);
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
        visual: aggregate_visual_evidence(&visual_samples, start, end - start),
    }
}

#[cfg(test)]
mod tests;

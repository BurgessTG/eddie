//! Lightweight QC analysis for imported footage.
//!
//! The sweep samples decoded frames at a small size, scores frame-to-frame
//! motion for stability and scene changes, then joins those samples with the
//! already-computed audio peaks. It returns edit-sized segments only; ranking
//! and user-facing reasons live in the app layer.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;

use crate::decode::VideoDecoder;
use crate::{run, MediaInfo, MediaResult, RawFrame};

/// Per-frame local visual measurements at sweep resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameVisualEvidence {
    /// Share of sampled pixels that changed materially from the
    /// previous frame. This is foreground/motion area, not a mask.
    pub foreground_coverage: f32,
    pub foreground_center_x: Option<f32>,
    pub foreground_center_y: Option<f32>,
    /// Share of local gradient/foreground saliency in the central
    /// part of the frame, 0..1.
    pub center_saliency: f32,
    pub edge_density: f32,
    pub luminance: f32,
    pub contrast: f32,
    /// Share of sampled pixels in a broad warm skin-tone range.
    /// Lighting and wooden/warm backgrounds can fool it; it is
    /// never face or person evidence by itself.
    pub skin_tone_ratio: f32,
}

/// Segment-level visual evidence: measured proxies from frame analysis,
/// not a person, face, expression or mask detector. Curations saved with
/// the removed SAM fields still load; serde ignores them.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SegmentVisualEvidence {
    /// Mean foreground area from frame differencing.
    pub subject_coverage: f32,
    pub subject_center_x: Option<f32>,
    pub subject_center_y: Option<f32>,
    /// Foreground-centroid travel per second, normalized to frame
    /// dimensions and capped at 1.
    pub subject_motion: f32,
    pub center_saliency: f32,
    pub edge_density: f32,
    pub luminance: f32,
    pub contrast: f32,
    pub skin_tone_ratio: f32,
}

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
    let output = run(Command::new("ffmpeg").args([
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

fn pixel_luminance(frame: &[u8], index: usize) -> f32 {
    if index + 2 >= frame.len() {
        return 0.0;
    }
    (0.2126 * frame[index] as f32
        + 0.7152 * frame[index + 1] as f32
        + 0.0722 * frame[index + 2] as f32)
        / 255.0
}

fn is_skin_tone(red: u8, green: u8, blue: u8) -> bool {
    let (red, green, blue) = (red as i32, green as i32, blue as i32);
    red >= 45
        && green >= 30
        && blue >= 20
        && red > green
        && green >= blue
        && (5..=90).contains(&(red - green))
        && (10..=140).contains(&(red - blue))
        && red - blue >= 15
        && (red * 10) <= blue * 24
}

/// Analyze one small RGBA frame. The result is a set of measured
/// proxies: foreground change, center emphasis, texture, exposure,
/// and warm skin-tone area. It deliberately does not claim a
/// person, face, expression, or segmentation mask.
pub fn frame_visual_evidence(
    previous: Option<&[u8]>,
    current: &[u8],
    width: u32,
    height: u32,
) -> FrameVisualEvidence {
    let width = width as usize;
    let height = height as usize;
    if width == 0 || height == 0 || current.len() < width.saturating_mul(height).saturating_mul(4) {
        return FrameVisualEvidence::default();
    }
    let step = if width > 32 || height > 32 { 2 } else { 1 };
    let mut samples = 0_usize;
    let mut luminance_sum = 0.0_f32;
    let mut luminance_sq_sum = 0.0_f32;
    let mut edge_count = 0_usize;
    let mut skin_count = 0_usize;
    let mut foreground_count = 0_usize;
    let mut foreground_weight = 0.0_f32;
    let mut foreground_x = 0.0_f32;
    let mut foreground_y = 0.0_f32;
    let mut saliency_sum = 0.0_f32;
    let mut center_saliency_sum = 0.0_f32;

    for y in (0..height).step_by(step) {
        for x in (0..width).step_by(step) {
            let index = (y * width + x) * 4;
            let luminance = pixel_luminance(current, index);
            luminance_sum += luminance;
            luminance_sq_sum += luminance * luminance;
            samples += 1;
            if is_skin_tone(current[index], current[index + 1], current[index + 2]) {
                skin_count += 1;
            }

            let mut gradient = 0.0_f32;
            if x + step < width {
                gradient = gradient
                    .max((luminance - pixel_luminance(current, (y * width + x + step) * 4)).abs());
            }
            if y + step < height {
                gradient = gradient.max(
                    (luminance - pixel_luminance(current, ((y + step) * width + x) * 4)).abs(),
                );
            }
            if gradient >= 0.18 {
                edge_count += 1;
            }

            let difference = previous
                .filter(|previous| previous.len() >= current.len())
                .map(|previous| {
                    ((previous[index] as i32 - current[index] as i32).unsigned_abs()
                        + (previous[index + 1] as i32 - current[index + 1] as i32).unsigned_abs()
                        + (previous[index + 2] as i32 - current[index + 2] as i32).unsigned_abs())
                        as f32
                        / (3.0 * 255.0)
                })
                .unwrap_or(0.0);
            let normalized_x = (x as f32 + 0.5) / width as f32;
            let normalized_y = (y as f32 + 0.5) / height as f32;
            if difference >= 0.09 {
                foreground_count += 1;
                foreground_weight += difference;
                foreground_x += normalized_x * difference;
                foreground_y += normalized_y * difference;
            }
            let saliency = gradient + difference;
            saliency_sum += saliency;
            if (0.25..=0.75).contains(&normalized_x) && (0.2..=0.8).contains(&normalized_y) {
                center_saliency_sum += saliency;
            }
        }
    }

    if samples == 0 {
        return FrameVisualEvidence::default();
    }
    let luminance = luminance_sum / samples as f32;
    let variance = (luminance_sq_sum / samples as f32 - luminance * luminance).max(0.0);
    FrameVisualEvidence {
        foreground_coverage: foreground_count as f32 / samples as f32,
        foreground_center_x: (foreground_weight > 0.0)
            .then_some((foreground_x / foreground_weight).clamp(0.0, 1.0)),
        foreground_center_y: (foreground_weight > 0.0)
            .then_some((foreground_y / foreground_weight).clamp(0.0, 1.0)),
        center_saliency: if saliency_sum > 0.0 {
            (center_saliency_sum / saliency_sum).clamp(0.0, 1.0)
        } else {
            0.0
        },
        edge_density: edge_count as f32 / samples as f32,
        luminance: luminance.clamp(0.0, 1.0),
        contrast: variance.sqrt().clamp(0.0, 1.0),
        skin_tone_ratio: skin_count as f32 / samples as f32,
    }
}

fn aggregate_visual_evidence(
    samples: &[&MotionSample],
    duration: f64,
) -> Option<SegmentVisualEvidence> {
    if samples.is_empty() {
        return None;
    }
    let count = samples.len() as f32;
    let mean = |pick: &dyn Fn(&FrameVisualEvidence) -> f32| {
        samples
            .iter()
            .map(|sample| pick(&sample.visual))
            .sum::<f32>()
            / count
    };
    let mut center_weight = 0.0_f32;
    let mut center_x_sum = 0.0_f32;
    let mut center_y_sum = 0.0_f32;
    let mut center_count = 0_usize;
    let mut unweighted_x = 0.0_f32;
    let mut unweighted_y = 0.0_f32;
    let mut travel = 0.0_f32;
    let mut previous_center: Option<(f32, f32)> = None;
    for sample in samples {
        if let (Some(x), Some(y)) = (
            sample.visual.foreground_center_x,
            sample.visual.foreground_center_y,
        ) {
            center_count += 1;
            unweighted_x += x;
            unweighted_y += y;
            let weight = sample.visual.foreground_coverage;
            center_weight += weight;
            center_x_sum += x * weight;
            center_y_sum += y * weight;
            if let Some((previous_x, previous_y)) = previous_center {
                travel += ((x - previous_x).powi(2) + (y - previous_y).powi(2)).sqrt();
            }
            previous_center = Some((x, y));
        } else {
            previous_center = None;
        }
    }
    let (subject_center_x, subject_center_y) = if center_weight > 0.0 {
        (
            Some((center_x_sum / center_weight).clamp(0.0, 1.0)),
            Some((center_y_sum / center_weight).clamp(0.0, 1.0)),
        )
    } else if center_count > 0 {
        (
            Some((unweighted_x / center_count as f32).clamp(0.0, 1.0)),
            Some((unweighted_y / center_count as f32).clamp(0.0, 1.0)),
        )
    } else {
        (None, None)
    };

    Some(SegmentVisualEvidence {
        subject_coverage: mean(&|visual| visual.foreground_coverage),
        subject_center_x,
        subject_center_y,
        subject_motion: if duration > 0.0 {
            (travel / duration as f32).clamp(0.0, 1.0)
        } else {
            0.0
        },
        center_saliency: mean(&|visual| visual.center_saliency),
        edge_density: mean(&|visual| visual.edge_density),
        luminance: mean(&|visual| visual.luminance),
        contrast: mean(&|visual| visual.contrast),
        skin_tone_ratio: mean(&|visual| visual.skin_tone_ratio),
    })
}

const MIN_SCENE_SCORE: f32 = 0.25;

fn scene_score(
    pixel_difference: f32,
    histogram_difference: f32,
    block_histogram_difference: f32,
) -> f32 {
    // Global palettes repeat across shots in one location (the
    // same carpet/walls/screens), so global color alone misses
    // clean montage cuts. A 4x4 block histogram preserves rough
    // layout while tolerating small in-shot movement. Gate its
    // full weight on a material pixel jump so a pan through a
    // textured room does not become a cut by itself.
    let block_term = if pixel_difference >= 0.12 {
        block_histogram_difference * 0.85
    } else {
        block_histogram_difference * 0.45
    };
    histogram_difference
        .max(pixel_difference * 0.55)
        .max(block_term)
        .clamp(0.0, 1.0)
}

#[derive(Default)]
struct SceneContinuity {
    previous: Vec<[f32; 3]>,
    current: Vec<[f32; 3]>,
}

#[derive(Clone, Copy, Default)]
struct SceneMatch {
    correlation: f32,
    dx: i32,
    dy: i32,
    texture: f32,
}

impl SceneContinuity {
    fn score_weight(
        &mut self,
        previous: &[u8],
        current: &[u8],
        width: usize,
        height: usize,
    ) -> f32 {
        if width < 16 || height < 8 || previous.len() != current.len() {
            return 1.0;
        }
        // Low-pass before matching. Native 64px fast scaling aliases fine
        // texture, even when the camera has only moved a few pixels.
        smooth_scene_frame(previous, width, height, &mut self.previous);
        smooth_scene_frame(current, width, height, &mut self.current);
        let mut matches = [SceneMatch::default(); 16];
        let mut previous_texture = 0.0;
        let mut current_texture = 0.0;
        for (index, best) in matches.iter_mut().enumerate() {
            let x = (((index % 4 * 2 + 1) * width / 4).saturating_sub(ScenePatch::WIDTH) / 2)
                .min(width - ScenePatch::WIDTH);
            let y = (((index / 4 * 2 + 1) * height / 4).saturating_sub(ScenePatch::HEIGHT) / 2)
                .min(height - ScenePatch::HEIGHT);
            let patch = ScenePatch::read(&self.current, width, x, y);
            current_texture += patch.spatial_energy;
            previous_texture += ScenePatch::read(&self.previous, width, x, y).spatial_energy;
            if !patch.has_texture() {
                continue;
            }
            for py in y.saturating_sub(8)..=(y + 8).min(height - ScenePatch::HEIGHT) {
                for px in x.saturating_sub(8)..=(x + 8).min(width - ScenePatch::WIDTH) {
                    let before = ScenePatch::read(&self.previous, width, px, py);
                    let correlation = patch.correlation(&before);
                    let dx = px as i32 - x as i32;
                    let dy = py as i32 - y as i32;
                    if correlation > best.correlation + 1e-6
                        || ((correlation - best.correlation).abs() <= 1e-6
                            && dx.abs() + dy.abs() < best.dx.abs() + best.dy.abs())
                    {
                        *best = SceneMatch {
                            correlation,
                            dx,
                            dy,
                            texture: patch.spatial_energy.min(before.spatial_energy),
                        };
                    }
                }
            }
        }

        let strong = matches.iter().filter(|m| m.correlation >= 0.9).count();
        let preserved = matches.iter().filter(|m| m.correlation >= 0.8).count();
        let preserved_texture: f32 = matches
            .iter()
            .filter(|m| m.correlation >= 0.8)
            .map(|m| m.texture)
            .sum();
        // Exposure changes preserve textured layout under a positive gain
        // and offset. Normalize to the less textured frame so an occluder
        // leaving does not make the retained background disappear statistically.
        // Half a shared layout alone must not erase a same-palette cut.
        if preserved >= 8
            && strong >= 4
            && preserved_texture > previous_texture.min(current_texture) * 0.55
        {
            return 0.2;
        }
        let mut tracked = 0;
        for anchor in &matches {
            if anchor.correlation < 0.9 || anchor.dx.abs().max(anchor.dy.abs()) < 2 {
                continue;
            }
            let aligned =
                |m: &&SceneMatch| (m.dx - anchor.dx).abs() <= 2 && (m.dy - anchor.dy).abs() <= 2;
            let strong = matches
                .iter()
                .filter(aligned)
                .filter(|m| m.correlation >= 0.9)
                .count();
            let supporting = matches
                .iter()
                .filter(aligned)
                .filter(|m| m.correlation >= 0.6)
                .count();
            if strong >= 2 && supporting >= 4 {
                tracked = tracked.max(strong);
            }
        }
        // A moving foreground can uncover a new palette while the background
        // still tracks. Weight the matched share of the half-grid support
        // target, but retain at least 60% of strong replacement evidence.
        // This never merges short scenes.
        (1.0 - tracked as f32 / 8.0).max(0.6)
    }
}

/// Below this share of strong-gradient pixels, a frame shows no fine
/// structure: typically a defocused foreground covering the lens.
const FEATURELESS_DETAIL: f32 = 0.02;
/// The view before the occlusion must carry at least three times that detail.
const OCCLUSION_ANCHOR_DETAIL: f32 = FEATURELESS_DETAIL * 3.0;
/// The anchor precedes the featureless run by up to this long, so it
/// is not already partly covered by the entering occluder.
const OCCLUSION_LOOKBACK: f64 = 0.5;
/// Longer featureless intervals are not treated as a passing occluder.
const OCCLUSION_MAX_RUN: f64 = 2.0;
/// An exiting occluder can linger at the frame edge before the anchor
/// view is fully visible again.
const OCCLUSION_SETTLE: f64 = 1.0;

/// A foreground object passing close to the lens replaces the picture
/// with featureless blur, then reveals the same view. Adjacent frames see
/// replacements, but comparing the view before the run with the view after
/// it sees one shot. Only an excursion entered or left through a partly
/// covered frame, and bracketed by matching detailed anchors, is reduced,
/// and only to the across-excursion score. A detailed insert never starts
/// an excursion, and a featureless run between different views keeps its
/// boundaries.
#[derive(Default)]
struct OcclusionExcursion {
    /// Recent sample indices and their 64 px frames, at most the lookback.
    recent: VecDeque<(usize, Vec<u8>)>,
    anchor: Vec<u8>,
    anchor_histograms: Option<Box<Histograms>>,
    run: Option<OcclusionRun>,
    previous_featureless: bool,
}

struct OcclusionRun {
    anchor: usize,
    start_time: f64,
    /// Time of the first detailed sample after the featureless run.
    revealed: Option<f64>,
    /// A partly covered view beside the run keeps under three quarters of
    /// the anchor's detail.
    partial_detail: f32,
    /// An occluder covers or uncovers the view across frames. A hard cut
    /// to a flat card and back is instant on both sides.
    gradual: bool,
}

impl OcclusionExcursion {
    fn observe(
        &mut self,
        samples: &mut [MotionSample],
        frame: &RawFrame,
        histograms: &Histograms,
        continuity: &mut SceneContinuity,
    ) {
        let Some(index) = samples.len().checked_sub(1) else {
            return;
        };
        let (time, detail) = (samples[index].time, samples[index].visual.edge_density);
        let featureless = detail < FEATURELESS_DETAIL;

        if let Some(run) = &mut self.run {
            if featureless {
                run.revealed = None;
                if time - run.start_time > OCCLUSION_MAX_RUN {
                    self.run = None;
                }
            } else {
                if run.revealed.is_none() {
                    run.gradual |= detail < run.partial_detail;
                    run.revealed = Some(time);
                }
                let revealed = run.revealed.unwrap_or(time);
                if !run.gradual || time - revealed > OCCLUSION_SETTLE {
                    self.run = None;
                } else if detail >= OCCLUSION_ANCHOR_DETAIL {
                    let excursion = &mut samples[run.anchor + 1..];
                    if excursion
                        .iter()
                        .any(|sample| sample.scene_score >= MIN_SCENE_SCORE)
                    {
                        if let Some(anchor_histograms) = &self.anchor_histograms {
                            let mut across = scene_score(
                                frame_difference(&self.anchor, &frame.rgba),
                                histogram_difference(&anchor_histograms.0, &histograms.0),
                                block_histogram_difference(&anchor_histograms.1, &histograms.1),
                            );
                            if across >= MIN_SCENE_SCORE {
                                across *= continuity.score_weight(
                                    &self.anchor,
                                    &frame.rgba,
                                    frame.width as usize,
                                    frame.height as usize,
                                );
                            }
                            if across < MIN_SCENE_SCORE {
                                for sample in excursion {
                                    sample.scene_score = sample.scene_score.min(across);
                                }
                                self.run = None;
                            }
                        }
                    } else {
                        self.run = None;
                    }
                }
            }
        } else if featureless && !self.previous_featureless {
            if let Some((anchor, pixels)) = self.recent.front() {
                let anchor_detail = samples[*anchor].visual.edge_density;
                if anchor_detail >= OCCLUSION_ANCHOR_DETAIL && pixels.len() == frame.rgba.len() {
                    let partial_detail = anchor_detail * 0.75;
                    self.anchor.clear();
                    self.anchor.extend_from_slice(pixels);
                    self.anchor_histograms =
                        Some(Box::new(rgb_histograms(pixels, frame.width, frame.height)));
                    self.run = Some(OcclusionRun {
                        anchor: *anchor,
                        start_time: time,
                        revealed: None,
                        partial_detail,
                        gradual: index > 0
                            && samples[index - 1].visual.edge_density < partial_detail,
                    });
                }
            }
        }
        self.previous_featureless = featureless;

        let mut buffer = Vec::new();
        while let Some((oldest, _)) = self.recent.front() {
            if time - samples[*oldest].time <= OCCLUSION_LOOKBACK {
                break;
            }
            if let Some((_, old)) = self.recent.pop_front() {
                buffer = old;
            }
        }
        buffer.clear();
        buffer.extend_from_slice(&frame.rgba);
        self.recent.push_back((index, buffer));
    }
}

fn smooth_scene_frame(frame: &[u8], width: usize, height: usize, output: &mut Vec<[f32; 3]>) {
    output.resize(width * height, [0.0; 3]);
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0.0; 3];
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let px = (x as isize + dx).clamp(0, width as isize - 1) as usize;
                    let py = (y as isize + dy).clamp(0, height as isize - 1) as usize;
                    for channel in 0..3 {
                        sum[channel] += frame[(py * width + px) * 4 + channel] as f32;
                    }
                }
            }
            output[y * width + x] = sum.map(|value| value / 9.0);
        }
    }
}

struct ScenePatch {
    pixels: [[f32; 3]; Self::WIDTH * Self::HEIGHT],
    sums: [f32; 3],
    energy: f32,
    spatial_energy: f32,
}

impl ScenePatch {
    const WIDTH: usize = 12;
    const HEIGHT: usize = 7;
    const PIXELS: f32 = (Self::WIDTH * Self::HEIGHT) as f32;

    fn read(frame: &[[f32; 3]], width: usize, x: usize, y: usize) -> Self {
        let mut pixels = [[0.0; 3]; Self::WIDTH * Self::HEIGHT];
        let mut sums = [0.0; 3];
        let mut squares = 0.0;
        for (index, pixel) in pixels.iter_mut().enumerate() {
            *pixel = frame[(y + index / Self::WIDTH) * width + x + index % Self::WIDTH];
            for channel in 0..3 {
                sums[channel] += pixel[channel];
                squares += pixel[channel] * pixel[channel];
            }
        }
        let total: f32 = sums.iter().sum();
        Self {
            pixels,
            sums,
            energy: (squares - total * total / (Self::PIXELS * 3.0)).max(0.0),
            spatial_energy: (squares - sums.iter().map(|s| s * s).sum::<f32>() / Self::PIXELS)
                .max(0.0),
        }
    }

    fn has_texture(&self) -> bool {
        self.spatial_energy >= 8.0 * 8.0 * Self::PIXELS * 3.0
    }

    fn correlation(&self, other: &Self) -> f32 {
        if !other.has_texture() {
            return 0.0;
        }
        let product: f32 = self
            .pixels
            .iter()
            .zip(&other.pixels)
            .map(|(a, b)| a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>())
            .sum();
        let color_covariance = product
            - self.sums.iter().sum::<f32>() * other.sums.iter().sum::<f32>() / (Self::PIXELS * 3.0);
        let spatial_covariance = product
            - self
                .sums
                .iter()
                .zip(other.sums)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                / Self::PIXELS;
        // Require both color and within-channel geometry to agree. A flat
        // shared palette is not evidence of the same objects or camera view.
        (color_covariance / (self.energy * other.energy).sqrt())
            .min(spatial_covariance / (self.spatial_energy * other.spatial_energy).sqrt())
            .clamp(0.0, 1.0)
    }
}

#[cfg(test)]
fn rgb_histogram(frame: &[u8]) -> [f32; 64] {
    rgb_histograms(frame, 0, 0).0
}

/// A frame's whole-picture RGB histogram and its 4x4 block histograms.
type Histograms = ([f32; 64], [[f32; 64]; 16]);

/// Global plus 4x4-block RGB histograms, sampled identically so
/// their differences are directly comparable from frame to frame.
fn rgb_histograms(frame: &[u8], width: u32, height: u32) -> Histograms {
    let mut histogram = [0.0_f32; 64];
    let mut blocks = [[0.0_f32; 64]; 16];
    let mut block_counts = [0.0_f32; 16];
    let mut count = 0.0_f32;
    let width = width as usize;
    let height = height as usize;
    let mut index = 0_usize;
    while index + 2 < frame.len() {
        // Interpolate between bin centers. Hard bins turn a one-level
        // exposure change (127 -> 128) into a complete palette replacement.
        let channels = [frame[index], frame[index + 1], frame[index + 2]].map(|value| {
            let position = (value as f32 / 64.0 - 0.5).clamp(0.0, 3.0);
            let lower = position.floor() as usize;
            let fraction = position - lower as f32;
            [(lower, 1.0 - fraction), ((lower + 1).min(3), fraction)]
        });
        let block = if width > 0 && height > 0 {
            let pixel = index / 4;
            let x = pixel % width;
            let y = pixel / width;
            (y < height).then_some((y * 4 / height) * 4 + (x * 4 / width))
        } else {
            None
        };
        for (red, red_weight) in channels[0] {
            for (green, green_weight) in channels[1] {
                for (blue, blue_weight) in channels[2] {
                    let bin = red * 16 + green * 4 + blue;
                    let weight = red_weight * green_weight * blue_weight;
                    histogram[bin] += weight;
                    if let Some(block) = block {
                        blocks[block][bin] += weight;
                    }
                }
            }
        }
        count += 1.0;
        if let Some(block) = block {
            block_counts[block] += 1.0;
        }
        index += 4 * 4;
    }
    if count > 0.0 {
        for bin in &mut histogram {
            *bin /= count;
        }
    }
    for (block, block_count) in blocks.iter_mut().zip(block_counts.iter()) {
        if *block_count > 0.0 {
            for bin in block.iter_mut() {
                *bin /= *block_count;
            }
        }
    }
    (histogram, blocks)
}

fn block_histogram_difference(previous: &[[f32; 64]; 16], current: &[[f32; 64]; 16]) -> f32 {
    let total: f32 = previous
        .iter()
        .zip(current.iter())
        .map(|(before, after)| histogram_difference(before, after))
        .sum();
    (total / previous.len() as f32).clamp(0.0, 1.0)
}

fn histogram_difference(previous: &[f32; 64], current: &[f32; 64]) -> f32 {
    let total: f32 = previous
        .iter()
        .zip(current.iter())
        .map(|(before, after)| (before - after).abs())
        .sum();
    (total * 0.5).clamp(0.0, 1.0)
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

/// Detect frame-accurate visual cuts from per-frame scene scores.
///
/// A trailing median/MAD of neighboring score maxima adapts to busy
/// footage without treating repeated frames as high variance. It looks
/// back one second, but never past the last boundary: a shot is judged
/// against itself, not against the shot before it. Candidate picks group
/// nearby transitions; the complete scene inventory keeps every detected
/// boundary.
pub fn detect_scene_cuts(motion: &[MotionSample]) -> Vec<f64> {
    detect_scene_cuts_grouped(motion, 0.25)
}

fn detect_scene_cuts_grouped(motion: &[MotionSample], group_window: f64) -> Vec<f64> {
    let mut candidates: Vec<(f64, f32)> = Vec::new();
    // The first frame of the shot being judged. Scores from before it,
    // and the boundary's own score, say nothing about this shot's motion.
    let mut shot_start = 0;
    for (index, sample) in motion.iter().enumerate().skip(1) {
        if sample.scene_score < MIN_SCENE_SCORE || sample.time <= 0.0 {
            continue;
        }
        let history: Vec<f32> = motion[shot_start..index]
            .windows(2)
            .rev()
            .take_while(|pair| sample.time - pair[1].time <= 1.0)
            .map(|pair| pair[0].scene_score.max(pair[1].scene_score))
            .collect();
        let threshold = adaptive_scene_threshold(&history, MIN_SCENE_SCORE);
        if sample.scene_score >= threshold {
            candidates.push((sample.time, sample.scene_score));
            shot_start = index + 1;
        }
    }

    let mut cuts = Vec::new();
    let mut group: Vec<(f64, f32)> = Vec::new();
    for candidate in candidates {
        if let Some(last) = group.last() {
            if candidate.0 - last.0 > group_window {
                if let Some(best) = group
                    .iter()
                    .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.total_cmp(&a.0)))
                {
                    cuts.push(best.0);
                }
                group.clear();
            }
        }
        group.push(candidate);
    }
    if let Some(best) = group
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.total_cmp(&a.0)))
    {
        cuts.push(best.0);
    }
    cuts
}

fn adaptive_scene_threshold(history: &[f32], floor: f32) -> f32 {
    // A few prior transitions are not a motion baseline, especially in VFR
    // footage. Their median/MAD can otherwise hide the next distinct shot.
    if history.len() < 8 {
        return floor;
    }
    let baseline = median(history);
    let deviations: Vec<f32> = history
        .iter()
        .map(|value| (value - baseline).abs())
        .collect();
    let mad = median(&deviations);
    // A short shot with uneven motion has a wide MAD from few frames. A
    // score half again above everything in the shot is still a cut.
    let peak = history.iter().copied().fold(0.0, f32::max);
    floor.max((baseline + mad * 6.0).min(peak * 1.5))
}

fn median(values: &[f32]) -> f32 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    match sorted.len() {
        0 => 0.0,
        len if len % 2 == 1 => sorted[len / 2],
        len => (sorted[len / 2 - 1] + sorted[len / 2]) * 0.5,
    }
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
        visual: aggregate_visual_evidence(&visual_samples, end - start),
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

/// Decode a mono 16 kHz float PCM excerpt, the input local speech models
/// expect. Bounded by the caller's duration; never a whole-file decode.
pub fn pcm_mono_16k(path: &str, start: f64, duration: f64) -> MediaResult<Vec<f32>> {
    let output = run(Command::new("ffmpeg").args([
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
        "16000",
        "-f",
        "f32le",
        "-",
    ]))?;
    Ok(output
        .stdout
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect())
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
                scene_score: if i == 8 { 0.5 } else { 0.01 },
                visual: FrameVisualEvidence::default(),
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
    fn segmentation_cuts_close_visual_scene_changes() {
        // 10 fps samples for 6 seconds. Pixel movement is noisy in
        // places, but scene_score spikes only at true switches.
        let motion: Vec<MotionSample> = (0..=60)
            .map(|i| {
                let cut = matches!(i, 10 | 32 | 37);
                MotionSample {
                    time: i as f64 * 0.1,
                    difference: if matches!(i, 18 | 44) { 0.14 } else { 0.02 },
                    scene_score: if cut { 0.8 } else { 0.02 },
                    visual: FrameVisualEvidence::default(),
                }
            })
            .collect();
        let segments = build_segments(6.0, &motion, &[0.0; 120], 20);
        let starts: Vec<f64> = segments.iter().map(|segment| segment.start).collect();
        assert_eq!(starts, vec![0.0, 1.0, 3.2, 3.7]);
    }

    #[test]
    fn scene_cuts_survive_repeated_frame_cadence() {
        for repeats in [1, 2, 3] {
            let motion: Vec<MotionSample> = (0..90 * repeats)
                .map(|frame| {
                    let original = frame / repeats;
                    let repeated = frame % repeats != 0;
                    MotionSample {
                        time: frame as f64 / (30 * repeats) as f64,
                        difference: if repeated { 0.0 } else { 0.15 },
                        scene_score: if repeated {
                            0.0
                        } else if original == 50 {
                            0.38
                        } else {
                            0.19 + (original % 3) as f32 * 0.01
                        },
                        visual: FrameVisualEvidence::default(),
                    }
                })
                .collect();
            let scenes = build_scene_segments(3.0, &motion, &[], 50);
            let starts: Vec<_> = scenes.iter().map(|scene| scene.start).collect();
            assert_eq!(
                starts,
                [0.0, 50.0 / 30.0],
                "{repeats} copies per source frame"
            );
        }
    }

    /// Scores measured on real footage, one per frame after `lead` calm
    /// frames, as the scene starts they produce.
    fn scene_starts(fps: f64, lead: usize, scores: &[f32]) -> Vec<f64> {
        let motion: Vec<MotionSample> = std::iter::repeat_n(0.02, lead)
            .chain(scores.iter().copied())
            .chain(std::iter::repeat_n(0.015, 12))
            .enumerate()
            .map(|(frame, scene_score)| MotionSample {
                time: frame as f64 / fps,
                difference: scene_score,
                scene_score,
                visual: FrameVisualEvidence::default(),
            })
            .collect();
        let duration = motion.len() as f64 / fps;
        build_scene_segments(duration, &motion, &[], 50)
            .iter()
            .map(|scene| (scene.start * fps).round() / fps)
            .collect()
    }

    /// A 0.67 s hyperlapse shot: every frame is nearly a new picture. The
    /// calm shot before it must not set its baseline, or frames inside it
    /// pass as cuts and its real end, judged against both shots at once,
    /// does not.
    #[test]
    fn a_fast_motion_shot_ends_at_its_cut_not_inside_itself() {
        let starts = scene_starts(
            30.0,
            40,
            &[
                0.513, 0.193, 0.199, 0.234, 0.231, 0.191, 0.225, 0.245, 0.235, 0.088, 0.168, 0.217,
                0.255, 0.239, 0.278, 0.246, 0.259, 0.300, 0.186, 0.214, 0.542,
            ],
        );
        assert_eq!(starts, [0.0, 40.0 / 30.0, 60.0 / 30.0]);
    }

    /// A 0.23 s shot whose few frames vary widely still ends where a score
    /// stands half again above all of them.
    #[test]
    fn a_short_uneven_shot_keeps_its_cut() {
        let starts = scene_starts(
            60.0,
            80,
            &[
                0.484, 0.007, 0.088, 0.002, 0.140, 0.004, 0.210, 0.004, 0.141, 0.004, 0.075, 0.001,
                0.211, 0.009, 0.382,
            ],
        );
        assert_eq!(starts, [0.0, 80.0 / 60.0, 94.0 / 60.0]);
    }

    #[test]
    fn scene_segments_cover_every_shot_without_candidate_cap_splits() {
        // 20 fps for 12 seconds, including a two-frame insert and
        // a final 10-second shot. Scene breakdown must cover the
        // whole source contiguously and must not split the long
        // shot at the 8-second curator-candidate cap.
        let motion: Vec<MotionSample> = (0..=240)
            .map(|i| MotionSample {
                time: i as f64 * 0.05,
                difference: 0.02,
                scene_score: if matches!(i, 20 | 22 | 40) { 0.8 } else { 0.02 },
                visual: FrameVisualEvidence::default(),
            })
            .collect();
        let segments = build_scene_segments(12.0, &motion, &[0.0; 240], 20);
        let starts: Vec<f64> = segments.iter().map(|segment| segment.start).collect();
        assert_eq!(starts, vec![0.0, 1.0, 1.1, 2.0]);
        assert_eq!(segments.last().expect("last scene").end, 12.0);
        assert!((segments.last().expect("last scene").duration() - 10.0).abs() < 1e-9);
        for pair in segments.windows(2) {
            assert_eq!(
                pair[0].end, pair[1].start,
                "scene coverage must be contiguous"
            );
        }
    }

    #[test]
    fn scene_segments_preserve_one_frame_inserts_at_high_rates_and_vfr() {
        for fps in [30, 60, 120] {
            let motion: Vec<MotionSample> = (0..fps * 2)
                .map(|frame| MotionSample {
                    time: frame as f64 / fps as f64,
                    difference: 0.0,
                    scene_score: if frame == fps / 2 {
                        0.8
                    } else if frame == fps / 2 + 1 {
                        0.5
                    } else {
                        0.0
                    },
                    visual: FrameVisualEvidence::default(),
                })
                .collect();
            let scenes = build_scene_segments(2.0, &motion, &[], 50);
            let starts: Vec<_> = scenes.iter().map(|scene| scene.start).collect();
            assert_eq!(starts, [0.0, 0.5, (fps / 2 + 1) as f64 / fps as f64]);
            assert_eq!(scenes.last().unwrap().end, 2.0);
        }
        let motion: Vec<MotionSample> = [0.0, 0.1, 0.45, 0.455, 0.9, 1.9]
            .into_iter()
            .map(|time| MotionSample {
                time,
                difference: 0.0,
                scene_score: if time == 0.45 {
                    0.8
                } else if time == 0.455 {
                    0.5
                } else {
                    0.0
                },
                visual: FrameVisualEvidence::default(),
            })
            .collect();
        let scenes = build_scene_segments(2.0, &motion, &[], 50);
        let starts: Vec<_> = scenes.iter().map(|scene| scene.start).collect();
        assert_eq!(starts, [0.0, 0.45, 0.455]);
    }

    #[test]
    fn block_histogram_catches_same_palette_layout_cut() {
        let width = 64_usize;
        let height = 36_usize;
        let mut previous = vec![0_u8; width * height * 4];
        let mut current = vec![0_u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 4;
                if x < width / 2 {
                    previous[index] = 255;
                } else {
                    previous[index + 2] = 255;
                }
                if y < height / 2 {
                    current[index] = 255;
                } else {
                    current[index + 2] = 255;
                }
                previous[index + 3] = 255;
                current[index + 3] = 255;
            }
        }
        let (previous_global, previous_blocks) =
            rgb_histograms(&previous, width as u32, height as u32);
        let (current_global, current_blocks) =
            rgb_histograms(&current, width as u32, height as u32);
        let global_diff = histogram_difference(&previous_global, &current_global);
        let block_diff = block_histogram_difference(&previous_blocks, &current_blocks);
        assert!(
            global_diff < 0.01,
            "global palette is unchanged: {global_diff}"
        );
        assert!(block_diff > 0.4, "layout changed materially: {block_diff}");
        assert!(
            scene_score(
                frame_difference(&previous, &current),
                global_diff,
                block_diff
            ) > 0.35
        );
    }

    #[test]
    fn scene_score_uses_histogram_not_movement_alone() {
        // Same black/white palette with halves swapped: lots of
        // pixel movement, identical color histogram. It is motion,
        // not clear evidence of a new scene by itself.
        let mut previous = vec![0_u8; 128 * 4];
        let mut current = vec![255_u8; 128 * 4];
        for pixel in 0..64 {
            previous[pixel * 4] = 255;
            previous[pixel * 4 + 1] = 255;
            previous[pixel * 4 + 2] = 255;
            current[pixel * 4] = 0;
            current[pixel * 4 + 1] = 0;
            current[pixel * 4 + 2] = 0;
        }
        let previous_hist = rgb_histogram(&previous);
        let hist_diff = histogram_difference(&previous_hist, &rgb_histogram(&current));
        assert!(hist_diff < 0.01, "hist diff {hist_diff}");
        assert!(frame_difference(&previous, &current) > 0.4);
    }

    #[test]
    fn per_frame_detection_finds_known_hard_cuts() {
        let dir = std::env::temp_dir().join(format!("openreel_scene_cuts_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("known_cuts.mp4");
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=160x90:r=10:d=1",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=160x90:r=10:d=2",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=160x90:r=10:d=0.5",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=160x90:r=10:d=2.5",
                "-filter_complex",
                "[0:v][1:v][2:v][3:v]concat=n=4:v=1:a=0[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("generate known-cut video");
        assert!(
            output.status.success(),
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let info = crate::probe(path.to_str().unwrap()).expect("probe known cuts");
        let samples = motion_samples(path.to_str().unwrap(), &info).expect("motion samples");
        assert!(
            (55..=65).contains(&samples.len()),
            "expected per-frame samples, got {}",
            samples.len()
        );
        let cuts = detect_scene_cuts(&samples);
        for expected in [1.0, 3.0, 3.5] {
            assert!(
                cuts.iter().any(|cut| (cut - expected).abs() <= 0.15),
                "missing cut near {expected}: {cuts:?}"
            );
        }
        assert_eq!(cuts.len(), 3, "unexpected extra cuts: {cuts:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn per_frame_detection_rejects_smooth_lighting_and_camera_pan() {
        let dir =
            std::env::temp_dir().join(format!("openreel_continuous_shots_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        for (name, filter) in [
            ("lighting", "geq=lum='30+2*N':cb=128:cr=128"),
            (
                "pan",
                "geq=lum='30+190*mod(floor((X+2*N)/12)+floor(Y/12),2)':cb=128:cr=128",
            ),
        ] {
            let path = dir.join(format!("{name}.mp4"));
            let output = Command::new("ffmpeg")
                .args([
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=s=160x90:r=30:d=3",
                    "-vf",
                    filter,
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-threads:v",
                    "1",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(&path)
                .output()
                .expect("generate continuous shot");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let info = crate::probe(path.to_str().unwrap()).expect("probe continuous shot");
            let samples = motion_samples(path.to_str().unwrap(), &info).expect("motion samples");
            let scenes = build_scene_segments(info.duration, &samples, &[], 50);
            assert_eq!(scenes.len(), 1, "{name} is one continuous shot: {scenes:?}");
            assert_eq!(scenes[0].start, 0.0);
            assert_eq!(scenes[0].end, info.duration);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn frame_visual_evidence_tracks_centered_foreground() {
        let width = 8_usize;
        let height = 8_usize;
        let previous = vec![0_u8; width * height * 4];
        let mut current = previous.clone();
        for y in 2..6 {
            for x in 2..6 {
                let index = (y * width + x) * 4;
                current[index] = 255;
                current[index + 1] = 255;
                current[index + 2] = 255;
                current[index + 3] = 255;
            }
        }

        let evidence =
            frame_visual_evidence(Some(&previous), &current, width as u32, height as u32);
        assert!(evidence.foreground_coverage > 0.15, "{evidence:?}");
        let center_x = evidence.foreground_center_x.expect("foreground center x");
        let center_y = evidence.foreground_center_y.expect("foreground center y");
        assert!((0.35..=0.65).contains(&center_x), "center x {center_x}");
        assert!((0.35..=0.65).contains(&center_y), "center y {center_y}");
        assert!(evidence.center_saliency > 0.5, "{evidence:?}");
        assert!(evidence.edge_density > 0.0, "{evidence:?}");
    }

    #[test]
    fn segment_visual_evidence_aggregates_subject_proxy() {
        let visual = FrameVisualEvidence {
            foreground_coverage: 0.25,
            foreground_center_x: Some(0.5),
            foreground_center_y: Some(0.45),
            center_saliency: 0.8,
            edge_density: 0.3,
            luminance: 0.6,
            contrast: 0.2,
            skin_tone_ratio: 0.1,
        };
        let motion: Vec<MotionSample> = (0..10)
            .map(|index| MotionSample {
                time: index as f64 * 0.1,
                difference: 0.02,
                scene_score: 0.0,
                visual,
            })
            .collect();
        let segments = build_segments(1.0, &motion, &[0.0; 20], 20);
        let evidence = segments[0]
            .visual
            .as_ref()
            .expect("video segment carries visual evidence");
        assert!((evidence.subject_coverage - 0.25).abs() < 0.001);
        assert!((evidence.subject_center_x.expect("center x") - 0.5).abs() < 0.001);
    }

    #[test]
    fn segmentation_handles_audio_only() {
        let segments = build_segments(9.0, &[], &[0.25; 180], 20);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[1].end, 9.0);
    }
}

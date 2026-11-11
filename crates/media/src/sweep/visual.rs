//! Local visual evidence: what each small frame measures, and those
//! measurements joined over a range.

use crate::MotionSample;

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
    /// Travel of the foreground centroid per second, between half-second
    /// averages, in frame dimensions and capped at 1. A steady shot of a
    /// busy subject reads about 0.05, a pan about 0.13.
    pub subject_motion: f32,
    pub center_saliency: f32,
    pub edge_density: f32,
    pub luminance: f32,
    pub contrast: f32,
    pub skin_tone_ratio: f32,
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

/// Subject travel is measured between foreground centroids averaged over
/// this long. Frame-to-frame the centroid of a difference mask jumps around
/// a still subject, which read as full-speed movement on nearly all footage.
const SUBJECT_WINDOW: f64 = 0.5;

/// A frame whose foreground covers less than this says nothing reliable
/// about where a subject is.
const SUBJECT_MIN_COVERAGE: f32 = 0.03;

/// `samples` are the pictures in `[start, start + duration)`, in time order.
pub(super) fn aggregate_visual_evidence(
    samples: &[&MotionSample],
    start: f64,
    duration: f64,
) -> Option<SegmentVisualEvidence> {
    if samples.is_empty() {
        return None;
    }
    // The picture at the range's first instant was compared with the one
    // before the range. At a cut that is the previous shot, so nearly the
    // whole frame reads as foreground. It is left out unless it is the only
    // picture (a one-frame insert), which then has no foreground evidence.
    let inside: Vec<&MotionSample> = samples
        .iter()
        .copied()
        .filter(|sample| start <= 0.0 || sample.time > start)
        .collect();
    let foreground_known = !inside.is_empty();
    let samples: &[&MotionSample] = if foreground_known { &inside } else { samples };
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
    // (window index, coverage, coverage-weighted x, coverage-weighted y)
    let mut windows: Vec<(i64, f32, f32, f32)> = Vec::new();
    for sample in samples.iter().filter(|_| foreground_known) {
        let (Some(x), Some(y)) = (
            sample.visual.foreground_center_x,
            sample.visual.foreground_center_y,
        ) else {
            continue;
        };
        center_count += 1;
        unweighted_x += x;
        unweighted_y += y;
        let weight = sample.visual.foreground_coverage;
        center_weight += weight;
        center_x_sum += x * weight;
        center_y_sum += y * weight;
        if weight < SUBJECT_MIN_COVERAGE {
            continue;
        }
        let index = ((sample.time - start) / SUBJECT_WINDOW).floor() as i64;
        match windows.last_mut() {
            Some(window) if window.0 == index => {
                window.1 += weight;
                window.2 += x * weight;
                window.3 += y * weight;
            }
            _ => windows.push((index, weight, x * weight, y * weight)),
        }
    }
    // Only neighbouring windows count: across a stretch without foreground
    // the subject may be a different one.
    let travel: f32 = windows
        .windows(2)
        .filter(|pair| pair[1].0 == pair[0].0 + 1)
        .map(|pair| {
            let (from, to) = (pair[0], pair[1]);
            (to.2 / to.1 - from.2 / from.1).hypot(to.3 / to.1 - from.3 / from.1)
        })
        .sum();
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
        subject_coverage: if foreground_known {
            mean(&|visual| visual.foreground_coverage)
        } else {
            0.0
        },
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

//! Scene-cut detection: per-frame histogram scores against an adaptive
//! baseline. Audio never creates a boundary.

use crate::MotionSample;

pub(super) const MIN_SCENE_SCORE: f32 = 0.25;

pub(super) fn scene_score(
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

pub(super) fn smooth_scene_frame(
    frame: &[u8],
    width: usize,
    height: usize,
    output: &mut Vec<[f32; 3]>,
) {
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

#[cfg(test)]
pub(super) fn rgb_histogram(frame: &[u8]) -> [f32; 64] {
    rgb_histograms(frame, 0, 0).0
}

/// A frame's whole-picture RGB histogram and its 4x4 block histograms.
pub(super) type Histograms = ([f32; 64], [[f32; 64]; 16]);

/// Global plus 4x4-block RGB histograms, sampled identically so
/// their differences are directly comparable from frame to frame.
pub(super) fn rgb_histograms(frame: &[u8], width: u32, height: u32) -> Histograms {
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

pub(super) fn block_histogram_difference(
    previous: &[[f32; 64]; 16],
    current: &[[f32; 64]; 16],
) -> f32 {
    let total: f32 = previous
        .iter()
        .zip(current.iter())
        .map(|(before, after)| histogram_difference(before, after))
        .sum();
    (total / previous.len() as f32).clamp(0.0, 1.0)
}

pub(super) fn histogram_difference(previous: &[f32; 64], current: &[f32; 64]) -> f32 {
    let total: f32 = previous
        .iter()
        .zip(current.iter())
        .map(|(before, after)| (before - after).abs())
        .sum();
    (total * 0.5).clamp(0.0, 1.0)
}

const DIFFERENCE_STRIDE: usize = 17;

pub(super) fn frame_difference(previous: &[u8], current: &[u8]) -> f32 {
    let len = previous.len().min(current.len());
    if len < 4 {
        return 0.0;
    }
    // Sample every 17th pixel. That is enough for shake/scene detection and
    // keeps a long recording's sweep proportional to decode, not analysis.
    // The stride shares no factor with the 64-pixel analysis width; 16 read
    // the same four columns of every row, one of them the picture's edge.
    let mut total = 0_u64;
    let mut count = 0_u64;
    let mut index = 0_usize;
    while index + 2 < len {
        total += (previous[index] as i32 - current[index] as i32).unsigned_abs() as u64;
        total += (previous[index + 1] as i32 - current[index + 1] as i32).unsigned_abs() as u64;
        total += (previous[index + 2] as i32 - current[index + 2] as i32).unsigned_abs() as u64;
        count += 3;
        index += DIFFERENCE_STRIDE * 4;
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

pub(super) fn detect_scene_cuts_grouped(motion: &[MotionSample], group_window: f64) -> Vec<f64> {
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

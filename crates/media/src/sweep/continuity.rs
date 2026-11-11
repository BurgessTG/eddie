//! Checks that turn down a high scene score: the same view under a
//! flash or a foreground exit, and a defocused pass across the lens.

use std::collections::VecDeque;

use super::scene::{
    block_histogram_difference, frame_difference, histogram_difference, rgb_histograms,
    scene_score, smooth_scene_frame, Histograms, MIN_SCENE_SCORE,
};
use crate::{MotionSample, RawFrame};

#[derive(Default)]
pub(super) struct SceneContinuity {
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
    pub(super) fn score_weight(
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
pub(super) struct OcclusionExcursion {
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
    pub(super) fn observe(
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

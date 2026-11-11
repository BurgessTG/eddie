//! Ducking: sound that drops under speech and comes back after it.

use crate::{Clip, ClipKind, Timeline};

/// Ducked sound plays at this gain under speech (about -12 dB).
pub const DUCK_LEVEL: f32 = 0.25;

/// Seconds before speech over which ducked sound goes down.
pub const DUCK_ATTACK: f64 = 0.2;

/// Seconds it stays down after speech ends.
pub const DUCK_HOLD: f64 = 0.3;

/// Seconds over which it comes back up.
pub const DUCK_RELEASE: f64 = 0.8;

/// Pauses shorter than a full recovery and dip stay down, so music does
/// not pump between phrases and neighbouring ramps never overlap.
const DUCK_MERGE: f64 = DUCK_ATTACK + DUCK_HOLD + DUCK_RELEASE;

/// How far down a ducking envelope is `offset` seconds into a clip, from 0
/// (unity) to 1 ([`DUCK_LEVEL`]), with raised-cosine ramps. `ranges` are
/// the sorted, merged speech ranges from [`Timeline::duck_ranges`].
pub fn duck_amount(offset: f64, ranges: &[(f64, f64)]) -> f64 {
    let index = ranges.partition_point(|&(_, end)| end + DUCK_HOLD + DUCK_RELEASE <= offset);
    let Some(&(start, end)) = ranges.get(index) else {
        return 0.0;
    };
    let ease = |x: f64| (1.0 - (x.clamp(0.0, 1.0) * std::f64::consts::PI).cos()) * 0.5;
    if offset < start - DUCK_ATTACK {
        0.0
    } else if offset < start {
        ease((offset - start + DUCK_ATTACK) / DUCK_ATTACK)
    } else if offset < end + DUCK_HOLD {
        1.0
    } else {
        1.0 - ease((offset - end - DUCK_HOLD) / DUCK_RELEASE)
    }
}

/// Linear gain of a ducking envelope at `offset` (see [`duck_amount`]).
/// The curve moves in decibels, so halfway down is half the dip.
pub fn duck_gain(offset: f64, ranges: &[(f64, f64)]) -> f32 {
    match duck_amount(offset, ranges) {
        0.0 => 1.0,
        amount => f64::from(DUCK_LEVEL).powf(amount) as f32,
    }
}

impl Timeline {
    /// Where `target` ducks, as sorted ranges in its own clip-local
    /// seconds: wherever another audible clip that does not itself duck
    /// is speaking. `speech` maps a source path to its audible
    /// source-time ranges. Empty unless `target.duck` is set.
    ///
    /// A range can begin before the clip or end after it. Speech just
    /// outside a clip still shapes the ramp inside it, so a piece of split
    /// music ducks exactly as the whole clip did.
    pub fn duck_ranges(
        &self,
        target: &Clip,
        speech: &std::collections::HashMap<String, Vec<(f64, f64)>>,
    ) -> Vec<(f64, f64)> {
        if !target.duck {
            return Vec::new();
        }
        let mut ranges: Vec<(f64, f64)> = Vec::new();
        for clip in &self.clips {
            let audible = clip.kind == ClipKind::Audio
                || (clip.kind == ClipKind::Video && clip.embedded_audio);
            if clip.id == target.id || clip.duck || !audible || clip.volume <= 0.0 {
                continue;
            }
            let Some(spans) = clip.source.as_ref().and_then(|source| speech.get(source)) else {
                continue;
            };
            let source_end = clip.source_in + clip.duration;
            let offset = clip.start - clip.source_in - target.start;
            for &(from, to) in spans {
                let (from, to) = (from.max(clip.source_in), to.min(source_end));
                if to > from {
                    ranges.push((from + offset, to + offset));
                }
            }
        }
        ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Merge on the whole timeline before choosing what reaches this
        // clip: a pause that stays down must not depend on where the clip
        // happens to end.
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (start, end) in ranges {
            match merged.last_mut() {
                Some(last) if start - last.1 < DUCK_MERGE => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged.retain(|&(start, end)| {
            end + DUCK_HOLD + DUCK_RELEASE > 0.0 && start - DUCK_ATTACK < target.duration
        });
        merged
    }
}

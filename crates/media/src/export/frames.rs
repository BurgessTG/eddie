//! The output clock, and the output frames each clip and title shows on.

use std::collections::{HashMap, HashSet};

use openreel_engine::{Clip, ClipId, Timeline};

#[derive(Clone, Copy)]
pub(super) struct VideoClock {
    pub(super) rate: ffmpeg_next::Rational,
    pub(super) tick_rate: i64,
    pub(super) frame_ticks: i64,
}

impl VideoClock {
    pub(super) fn new(rate: ffmpeg_next::Rational) -> Self {
        let tick_rate = ((i32::MAX / rate.numerator()) * rate.numerator()) as i64;
        Self {
            rate,
            tick_rate,
            frame_ticks: tick_rate / rate.numerator() as i64 * rate.denominator() as i64,
        }
    }

    /// Use the same integer clock as source offsets before rounding up to an
    /// output frame. Subtracting late source timestamps can leave more noise
    /// than a duration-relative epsilon permits, adding an unintended frame.
    /// Only sub-tick noise disappears; real off-grid boundaries still round up.
    pub(super) fn boundary(self, time: f64) -> i64 {
        let ticks = (time.max(0.0) * self.tick_rate as f64).round() as i64;
        ticks / self.frame_ticks + i64::from(ticks % self.frame_ticks != 0)
    }

    /// When output frame `frame` starts. The output rate need not be the
    /// timeline's.
    pub(super) fn time(self, frame: i64) -> f64 {
        frame as f64 / f64::from(self.rate)
    }
}

/// Output frame windows `[start, end)` over which nothing on the timeline
/// starts or ends, nor anything at `extra`, each with the time to sample
/// it at. What shows can change only between them.
fn windows(timeline: &Timeline, clock: VideoClock, extra: &[f64]) -> Vec<(i64, i64, f64)> {
    let mut boundaries = vec![0.0];
    for clip in &timeline.clips {
        boundaries.extend([clip.start, clip.end()]);
    }
    boundaries.extend_from_slice(extra);
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup();
    boundaries
        .windows(2)
        .filter_map(|pair| {
            let (start, end) = (clock.boundary(pair[0]), clock.boundary(pair[1]));
            (start < end).then(|| (start, end, pair[0].max(clock.time(start))))
        })
        .collect()
}

/// Add frames `[start, end)` to `ranges`, joining a range they continue.
fn push_frames(ranges: &mut Vec<(i64, i64)>, start: i64, end: i64) {
    match ranges.last_mut().filter(|last| last.1 == start) {
        Some(last) => last.1 = end,
        None => ranges.push((start, end)),
    }
}

/// The output frames each title shows on.
pub(super) fn title_frames(timeline: &Timeline, clock: VideoClock) -> FrameRanges {
    let mut frames: FrameRanges = HashMap::new();
    for (start, end, time) in windows(timeline, clock, &[]) {
        if let Some(clip) = timeline.active_title(time) {
            push_frames(frames.entry(clip.id).or_default(), start, end);
        }
    }
    frames
}

/// The output frames each clip's picture shows on, as its lane's picture
/// among those that show ([`Timeline::video_stack`]), and the clips drawn
/// over another lane's picture somewhere.
pub(super) fn visible_frames(
    timeline: &Timeline,
    clock: VideoClock,
    hides: &dyn Fn(&Clip) -> bool,
) -> (FrameRanges, HashSet<ClipId>) {
    let spans = timeline.transitions();
    let edges: Vec<f64> = spans
        .iter()
        .flat_map(|span| [span.start, span.end])
        .collect();
    let mut frames: FrameRanges = HashMap::new();
    let mut over = HashSet::new();
    for (start, end, time) in windows(timeline, clock, &edges) {
        for (depth, picture) in timeline.video_stack(&spans, time, hides).iter().enumerate() {
            push_frames(frames.entry(picture.clip.id).or_default(), start, end);
            if depth > 0 {
                over.insert(picture.clip.id);
                if let Some(span) = picture.transition {
                    over.insert(span.to);
                }
            }
        }
    }
    (frames, over)
}

/// Output frame ranges `[first, end)` per clip.
pub(super) type FrameRanges = HashMap<ClipId, Vec<(i64, i64)>>;

pub(super) fn enabled_frames(ranges: &[(i64, i64)]) -> String {
    match ranges {
        [] => "0".to_owned(),
        [(start, end)] => format!("gte(n,{start})*lt(n,{end})"),
        _ => {
            // A flat sum of hundreds of ranges exceeds AVExpr's parse depth.
            let (left, right) = ranges.split_at(ranges.len() / 2);
            format!("({}+{})", enabled_frames(left), enabled_frames(right))
        }
    }
}
